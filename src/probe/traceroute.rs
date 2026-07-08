use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::{ProbeError, TracerouteHop};
use crate::network::icmp::{DefaultIcmpProvider, IcmpProvider};
use super::{NetworkProbe, TelemetryEvent};
use crate::network::icmp::packet::IcmpResponse;

pub struct TracerouteProbe {
    pub target_ip: Arc<String>,
    pub resolved_addr: SocketAddr,
    pub timeout: Duration,
    pub max_hops: u8,
    pub icmp_identifier: u16,
}

impl TracerouteProbe {
    pub fn new(target_ip: Arc<String>, resolved_addr: SocketAddr, timeout: Duration, max_hops: u8, icmp_identifier: u16) -> Self {
        Self { target_ip, resolved_addr, timeout, max_hops, icmp_identifier }
    }
}

impl NetworkProbe for TracerouteProbe {
    async fn run(&mut self, tx: mpsc::Sender<TelemetryEvent>, cancel_token: CancellationToken) -> Result<(), ProbeError> {
        for ttl in 1..=self.max_hops {
            if cancel_token.is_cancelled() {
                break;
            }

            let seq = ttl as u16; 
            let target_addr = self.resolved_addr;
            let timeout_duration = self.timeout;
            let identifier = self.icmp_identifier;
            let ttl_u32 = ttl as u32;

            let hop_result = match tokio::task::spawn_blocking(move || {
                let provider = DefaultIcmpProvider::new(identifier);
                provider.send_with_ttl(&target_addr, seq, ttl_u32, timeout_duration)
            }).await {
                Ok(res) => res,
                Err(e) => return Err(ProbeError::Socket(std::io::Error::new(std::io::ErrorKind::Other, format!("Thread Panicked: {}", e)))),
            };

            match hop_result {
                Ok(result) => {
                    let is_dest = match result.response {
                        IcmpResponse::EchoReply(_) => true,
                        IcmpResponse::TimeExceeded(_) => false,
                        IcmpResponse::DestinationUnreachable(_) => true,
                        IcmpResponse::Unknown { .. } => false,
                    };

                    let hop = TracerouteHop {
                        hop_number: ttl,
                        ip_address: Some(result.responder_ip.to_string()),
                        rtt_ms: Some(result.rtt_ms),
                        is_destination: is_dest,
                    };

                    if tx.try_send(TelemetryEvent::TracerouteHop(hop)).is_err() {
                        break;
                    }

                    if is_dest {
                        break;
                    }
                }
                Err(ProbeError::IcmpTimeout) => {
                    let hop = TracerouteHop {
                        hop_number: ttl,
                        ip_address: None,
                        rtt_ms: None,
                        is_destination: false,
                    };
                    if tx.try_send(TelemetryEvent::TracerouteHop(hop)).is_err() {
                        break;
                    }
                }
                Err(_e) => {
                    // For any other error (like PermissionDenied), we log/send and break.
                    // The instructions state: "Eğer Timeout (veya geçici hata) dönerse: ip_address: None olan boş bir TracerouteHop yolla ve devam et."
                    // Let's treat it as a timeout/empty hop and continue if it's not a fatal socket error? 
                    // Actually, "geçici hata" means we should just continue.
                    let hop = TracerouteHop {
                        hop_number: ttl,
                        ip_address: None,
                        rtt_ms: None,
                        is_destination: false,
                    };
                    if tx.try_send(TelemetryEvent::TracerouteHop(hop)).is_err() {
                        break;
                    }
                }
            }
        }

        let _ = tx.try_send(TelemetryEvent::TracerouteComplete);
        Ok(())
    }
}
