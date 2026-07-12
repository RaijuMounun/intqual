#![cfg(unix)]
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use tokio::io::unix::AsyncFd;
use socket2::{Domain, Protocol, Socket, Type};

use super::provider::{IcmpProvider, TracerouteHopResult};
use super::packet::{IcmpEchoRequest, IcmpEchoReply, IcmpResponse};

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
    async fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::error!("Permission Denied creating raw socket: {}", e);
                return Err(ProbeError::PermissionDenied);
            }
            Err(e) => {
                tracing::error!("I/O Error creating socket: {}", e);
                return Err(ProbeError::Socket(e));
            }
        };

        if let Err(e) = socket.set_nonblocking(true) {
            tracing::error!("I/O Error setting nonblocking: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let async_fd = match AsyncFd::new(socket) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::error!("I/O Error creating AsyncFd: {}", e);
                return Err(ProbeError::Socket(e));
            }
        };

        let packet = IcmpEchoRequest::new(self.identifier, seq, vec![]);
        let packet_bytes = packet.encode_without_checksum();

        if let Err(e) = async_fd.get_ref().send_to(&packet_bytes, &(*target).into()) {
            tracing::error!("I/O Error sending packet: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let mut buf = [MaybeUninit::uninit(); 128];
        
        let timeout_future = tokio::time::timeout(timeout, async {
            loop {
                let mut guard = match async_fd.readable().await {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::error!("I/O Error awaiting readable: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                };

                match guard.try_io(|inner| inner.get_ref().recv_from(&mut buf)) {
                    Ok(Ok((size, _))) => {
                        let initialized_buf = unsafe {
                            std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                        };

                        if let Ok(reply) = IcmpEchoReply::decode(initialized_buf)
                            && reply.sequence_number == seq {
                                return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                        }
                    },
                    Ok(Err(e)) => {
                        tracing::error!("I/O Error on recv_from: {}", e);
                        return Err(ProbeError::Socket(e))
                    },
                    Err(_would_block) => continue,
                }
            }
        });

        match timeout_future.await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(e)) => {
                tracing::error!("I/O Error: {}", e);
                Err(e)
            }
            Err(e) => {
                tracing::warn!("Timeout: {}", e);
                Err(ProbeError::IcmpTimeout)
            }
        }
    }

    async fn send_with_ttl(&self, target: &SocketAddr, seq: u16, ttl: u32, timeout: Duration) -> Result<TracerouteHopResult, ProbeError> {
        let icmp_start = Instant::now();
        
        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::error!("Permission Denied creating raw socket: {}", e);
                return Err(ProbeError::PermissionDenied);
            }
            Err(e) => {
                tracing::error!("I/O Error creating socket: {}", e);
                return Err(ProbeError::Socket(e));
            }
        };

        if let Err(e) = socket.set_ttl_v4(ttl) {
            tracing::error!("I/O Error setting TTL: {}", e);
            return Err(ProbeError::Socket(e));
        }

        if let Err(e) = socket.set_nonblocking(true) {
            tracing::error!("I/O Error setting nonblocking: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let async_fd = match AsyncFd::new(socket) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::error!("I/O Error creating AsyncFd: {}", e);
                return Err(ProbeError::Socket(e));
            }
        };

        let packet = IcmpEchoRequest::new(self.identifier, seq, vec![]);
        let packet_bytes = packet.encode_without_checksum();

        if let Err(e) = async_fd.get_ref().send_to(&packet_bytes, &(*target).into()) {
            tracing::error!("I/O Error sending packet: {}", e);
            return Err(ProbeError::Socket(e));
        }

        let mut buf = [MaybeUninit::uninit(); 1500];
        
        let timeout_future = tokio::time::timeout(timeout, async {
            loop {
                let mut guard = match async_fd.readable().await {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::error!("I/O Error awaiting readable: {}", e);
                        return Err(ProbeError::Socket(e));
                    }
                };

                match guard.try_io(|inner| inner.get_ref().recv_from(&mut buf)) {
                    Ok(Ok((size, addr))) => {
                        let initialized_buf = unsafe {
                            std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                        };

                        if let Ok(response) = IcmpResponse::decode(initialized_buf) {
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
                            } else {
                                let got_id = match &response {
                                    IcmpResponse::EchoReply(r) => Some(r.identifier),
                                    IcmpResponse::TimeExceeded(t) => Some(t.original_identifier),
                                    IcmpResponse::DestinationUnreachable(d) => Some(d.original_identifier),
                                    IcmpResponse::Unknown { .. } => None,
                                };
                                if let Some(got) = got_id {
                                    tracing::debug!("Dropped packet: ID mismatch (Expected: {}, Got: {})", self.identifier, got);
                                }
                            }
                        }
                    },
                    Ok(Err(e)) => {
                        tracing::error!("I/O Error on recv_from: {}", e);
                        return Err(ProbeError::Socket(e))
                    },
                    Err(_would_block) => continue,
                }
            }
        });

        match timeout_future.await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(e)) => {
                tracing::error!("I/O Error: {}", e);
                Err(e)
            }
            Err(e) => {
                tracing::warn!("Timeout: {}", e);
                Err(ProbeError::IcmpTimeout)
            }
        }
    }
}
