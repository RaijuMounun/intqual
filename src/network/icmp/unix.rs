#[cfg(unix)]

use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use socket2::{Domain, Protocol, Socket, Type};

use super::provider::IcmpProvider;
use super::packet::{IcmpEchoRequest, IcmpEchoReply};

use crate::models::ProbeError;

pub struct UnixDgramIcmp {
    identifier: u16,
}

impl UnixDgramIcmp {
    pub fn new(identifier: u16) -> Self {
        Self { identifier }
    }
}

impl IcmpProvider for UnixDgramIcmp {
    fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(_) => return Err(ProbeError::PermissionDenied),
        };

        if let Err(e) = socket.set_read_timeout(Some(timeout)) {
            tracing::debug!("Failed to set read timeout (ignoring): {}", e);
        }
        if let Err(e) = socket.set_write_timeout(Some(timeout)) {
            tracing::debug!("Failed to set write timeout (ignoring): {}", e);
        }

        let packet = IcmpEchoRequest::new(self.identifier, seq, vec![]);
        let packet_bytes = packet.encode_without_checksum();

        if let Err(e) = socket.send_to(&packet_bytes, &(*target).into()) {
            return Err(ProbeError::Socket(e));
        }

        let mut buf = [MaybeUninit::uninit(); 128];
        
        loop {
            match socket.recv_from(&mut buf) {
                Ok((size, _)) => {
                    let initialized_buf = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                    };

                    if let Ok(reply) = IcmpEchoReply::decode(initialized_buf) {
                        if reply.sequence_number == seq {
                            return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                        }
                    }

                    if icmp_start.elapsed() > timeout {
                        return Err(ProbeError::IcmpTimeout);
                    }
                },
                Err(_) => return Err(ProbeError::IcmpTimeout),
            }
        }
    }
}
