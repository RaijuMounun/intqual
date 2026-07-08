#[cfg(target_os = "windows")]

use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use socket2::{Domain, Protocol, Socket, Type};

use super::provider::{IcmpProvider, TracerouteHopResult};
use super::packet::{IcmpEchoRequest, IcmpEchoReply, IcmpResponse};

use crate::models::ProbeError;

pub struct WindowsRawIcmp {
    identifier: u16,
}

impl WindowsRawIcmp {
    pub fn new(identifier: u16) -> Self {
        Self { identifier }
    }
}

impl IcmpProvider for WindowsRawIcmp {
    fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)) {
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
        let packet_bytes = packet.encode();

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
                        return Err(ProbeError::IcmpTimeout);
                    }
                },
                Err(_) => return Err(ProbeError::IcmpTimeout),
            }
        }
    }

    fn send_with_ttl(&self, target: &SocketAddr, seq: u16, ttl: u32, timeout: Duration) -> Result<TracerouteHopResult, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(_) => return Err(ProbeError::PermissionDenied),
        };

        if let Err(e) = socket.set_ttl_v4(ttl) {
            return Err(ProbeError::Socket(e));
        }

        if let Err(e) = socket.set_read_timeout(Some(timeout)) {
            tracing::debug!("Failed to set read timeout (ignoring): {}", e);
        }
        if let Err(e) = socket.set_write_timeout(Some(timeout)) {
            tracing::debug!("Failed to set write timeout (ignoring): {}", e);
        }

        let packet = IcmpEchoRequest::new(self.identifier, seq, vec![]);
        let packet_bytes = packet.encode();

        if let Err(e) = socket.send_to(&packet_bytes, &(*target).into()) {
            return Err(ProbeError::Socket(e));
        }

        let mut buf = [MaybeUninit::uninit(); 1500];
        
        loop {
            match socket.recv_from(&mut buf) {
                Ok((size, addr)) => {
                    let initialized_buf = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                    };

                    let response = IcmpResponse::decode(initialized_buf)
                        .or_else(|_| {
                            if size >= 28 {
                                IcmpResponse::decode(&initialized_buf[20..])
                            } else {
                                Err("Buffer too small")
                            }
                        });

                    if let Ok(response) = response {
                        let is_match = match &response {
                            IcmpResponse::EchoReply(r) => r.sequence_number == seq && r.identifier == self.identifier,
                            IcmpResponse::TimeExceeded(t) => t.original_sequence == seq && t.original_identifier == self.identifier,
                            IcmpResponse::DestinationUnreachable(d) => d.original_sequence == seq && d.original_identifier == self.identifier,
                            IcmpResponse::Unknown { .. } => false,
                        };

                        if is_match {
                            let responder_ip = addr.as_socket().map(|s| s.ip()).unwrap_or_else(|| target.ip());
                            return Ok(TracerouteHopResult {
                                rtt_ms: icmp_start.elapsed().as_secs_f64() * 1000.0,
                                responder_ip,
                                response,
                            });
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
