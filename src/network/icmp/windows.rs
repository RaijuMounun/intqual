#![cfg(target_os = "windows")]

use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use socket2::{Domain, Protocol, Socket, Type};

use super::provider::{IcmpProvider, TracerouteHopResult};
use super::packet::{IcmpEchoRequest, IcmpEchoReply, IcmpResponse};

use crate::models::ProbeError;

pub struct RawIcmpProvider {
    identifier: u16,
}

impl RawIcmpProvider {
    pub fn new(identifier: u16) -> Self {
        Self { identifier }
    }
}

impl IcmpProvider for RawIcmpProvider {
    async fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::error!("Permission Denied creating raw socket: {}", e);
                return Err(ProbeError::PermissionDenied);
            }
            Err(e) => {
                match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        tracing::warn!("Connection Refused creating socket: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                    std::io::ErrorKind::WouldBlock => {
                        tracing::warn!("WouldBlock creating socket: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                    _ => {
                        tracing::error!("Unclassified OS Network Error occurred: {:?}", e.kind());
                        return Err(ProbeError::Socket(e));
                    }
                }
            }
        };

        if let Err(e) = socket.set_read_timeout(Some(timeout)) {
            tracing::error!("I/O Error setting read timeout: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let packet = IcmpEchoRequest::new(self.identifier, seq, &[]);
        let packet_bytes = packet.encode();

        if let Err(e) = socket.send_to(&packet_bytes, &(*target).into()) {
            tracing::error!("I/O Error sending packet: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let identifier = self.identifier;
        let recv_task = tokio::task::spawn_blocking(move || {
            let mut buf = [MaybeUninit::uninit(); 128];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((size, _)) => {
                        let initialized_buf = unsafe {
                            std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                        };
                        let icmp_buf = IcmpResponse::strip_ipv4_header(initialized_buf);
                        match IcmpEchoReply::decode(icmp_buf) {
                            Ok(reply) => {
                                if reply.sequence_number == seq && reply.identifier == identifier {
                                    return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Dropped malformed or foreign ICMP packet: {}", e);
                                continue;
                            }
                        }
                    },
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
                            tracing::warn!("Timeout: OS recv_from timed out");
                            return Err(ProbeError::IcmpTimeout);
                        }
                        tracing::error!("I/O Error receiving packet: {}", e);
                        return Err(ProbeError::Socket(e))
                    },
                }
            }
        });

        match recv_task.await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(e)) => Err(e),
            Err(e) => {
                tracing::error!("Thread Panic: {}", e);
                Err(ProbeError::ThreadPanic(format!("Thread Panicked: {}", e)))
            }
        }
    }

    async fn send_with_ttl(&self, target: &SocketAddr, seq: u16, ttl: u32, timeout: Duration) -> Result<TracerouteHopResult, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::error!("Permission Denied creating raw socket: {}", e);
                return Err(ProbeError::PermissionDenied);
            }
            Err(e) => {
                match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        tracing::warn!("Connection Refused creating socket: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                    std::io::ErrorKind::WouldBlock => {
                        tracing::warn!("WouldBlock creating socket: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                    _ => {
                        tracing::error!("Unclassified OS Network Error occurred: {:?}", e.kind());
                        return Err(ProbeError::Socket(e));
                    }
                }
            }
        };

        if let Err(e) = socket.set_ttl_v4(ttl) {
            tracing::error!("I/O Error setting TTL: {}", e);
            return Err(ProbeError::Socket(e));
        }

        if let Err(e) = socket.set_read_timeout(Some(timeout)) {
            tracing::error!("I/O Error setting read timeout: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let packet = IcmpEchoRequest::new(self.identifier, seq, &[]);
        let packet_bytes = packet.encode();

        if let Err(e) = socket.send_to(&packet_bytes, &(*target).into()) {
            tracing::error!("I/O Error sending packet: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let target_clone = target.clone();
        let identifier = self.identifier;
        
        let recv_task = tokio::task::spawn_blocking(move || {
            let mut buf = [MaybeUninit::uninit(); 1500];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((size, addr)) => {
                        let initialized_buf = unsafe {
                            std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                        };

                        let icmp_buf = IcmpResponse::strip_ipv4_header(initialized_buf);
                        let response = IcmpResponse::decode(icmp_buf);
                        match response {
                            Ok(response) => {
                                let is_match = match &response {
                                    IcmpResponse::EchoReply(r) => r.sequence_number == seq && r.identifier == identifier,
                                    IcmpResponse::TimeExceeded(t) => t.original_sequence == seq && t.original_identifier == identifier,
                                    IcmpResponse::DestinationUnreachable(d) => d.original_sequence == seq && d.original_identifier == identifier,
                                    IcmpResponse::Unknown { .. } => false,
                                };
    
                                if is_match {
                                    let responder_ip = addr.as_socket().map(|s| s.ip()).unwrap_or_else(|| target_clone.ip());
                                    return Ok(TracerouteHopResult {
                                        rtt_ms: icmp_start.elapsed().as_secs_f64() * 1000.0,
                                        responder_ip,
                                        response,
                                    });
                                } else {
                                    let got_id = match &response {
                                        IcmpResponse::EchoReply(r) => Some(r.identifier),
                                        IcmpResponse::TimeExceeded(t) => Some(t.original_identifier),
                                        IcmpResponse::DestinationUnreachable(d) => Some(d.original_identifier),
                                        IcmpResponse::Unknown { .. } => None,
                                    };
                                    if let Some(got) = got_id {
                                        tracing::debug!("Dropped packet: ID mismatch (Expected: {}, Got: {})", identifier, got);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Dropped malformed or foreign ICMP packet: {}", e);
                                continue;
                            }
                        }
                    },
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
                            tracing::warn!("Timeout: OS recv_from timed out");
                            return Err(ProbeError::IcmpTimeout);
                        }
                        tracing::error!("I/O Error receiving packet: {}", e);
                        return Err(ProbeError::Socket(e))
                    },
                }
            }
        });

        match recv_task.await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(e)) => Err(e),
            Err(e) => {
                tracing::error!("Thread Panic: {}", e);
                Err(ProbeError::ThreadPanic(format!("Thread Panicked: {}", e)))
            }
        }
    }
}
