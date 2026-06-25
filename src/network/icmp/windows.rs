#[cfg(target_os = "windows")]

use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use socket2::{Domain, Protocol, Socket, Type};

use super::provider::IcmpProvider;
use super::packet::{IcmpEchoRequest, IcmpEchoReply};

pub struct WindowsRawIcmp {
    identifier: u16,
}

impl WindowsRawIcmp {
    pub fn new(identifier: u16) -> Self {
        Self { identifier }
    }
}

impl IcmpProvider for WindowsRawIcmp {
    fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, String> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(_) => return Err("Permission Denied / Unsupported".to_string()),
        };

        let _ = socket.set_read_timeout(Some(timeout));
        let _ = socket.set_write_timeout(Some(timeout));

        let packet = IcmpEchoRequest::new(self.identifier, seq, vec![]);
        let packet_bytes = packet.encode();

        if socket.send_to(&packet_bytes, &(*target).into()).is_err() {
            return Err("Send Failed".to_string());
        }

        let mut buf = [MaybeUninit::uninit(); 128];
        
        loop {
            match socket.recv_from(&mut buf) {
                Ok((size, _)) => {
                    let initialized_buf = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                    };

                    // For RAW sockets on Windows, the IP header (typically 20 bytes) might be included.
                    // However, we'll try decoding directly first, and if it fails, try with an offset,
                    // or just let the existing logic handle it as requested.
                    // The instruction said: "Keep the existing calculate_checksum logic and apply it before sending."
                    if let Ok(reply) = IcmpEchoReply::decode(initialized_buf) {
                        if reply.sequence_number == seq {
                            return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                        }
                    } else if size >= 28 {
                        // Attempt to decode skipping the 20-byte IP header
                        if let Ok(reply) = IcmpEchoReply::decode(&initialized_buf[20..]) {
                            if reply.sequence_number == seq {
                                return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                            }
                        }
                    }

                    if icmp_start.elapsed() > timeout {
                        return Err("ICMP Timeout".to_string());
                    }
                },
                Err(_) => return Err("ICMP Timeout".to_string()),
            }
        }
    }
}
