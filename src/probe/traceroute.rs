use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::{ProbeError, TracerouteHop};
use crate::network::icmp::{TracerouteIcmpProvider, IcmpProvider};
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

            let mut rtt_sum = 0.0;
            let mut rtt_count = 0;
            let mut last_responder = None;
            let mut is_dest_reached = false;
            let mut fatal_error = None;
            
            for probe_idx in 0..3 {
                let seq = (ttl as u16) * 3 + probe_idx; 
                let target_addr = self.resolved_addr;
                let timeout_duration = self.timeout;
                let identifier = self.icmp_identifier;
                let ttl_u32 = ttl as u32;

                let hop_result = {
                    tracing::debug!("Sending probe TTL={}, Seq={}, ID={}", ttl_u32, seq, identifier);
                    let provider = TracerouteIcmpProvider::new(identifier);
                    provider.send_with_ttl(&target_addr, seq, ttl_u32, timeout_duration).await
                };

                match hop_result {
                    Ok(result) => {
                        tracing::debug!("Received response for TTL={}: {:?}", ttl, result.response);
                        let is_dest = match result.response {
                            IcmpResponse::EchoReply(_) => true,
                            IcmpResponse::TimeExceeded(_) => false,
                            IcmpResponse::DestinationUnreachable(_) => true,
                            IcmpResponse::Unknown { .. } => false,
                        };
                        
                        rtt_sum += result.rtt_ms;
                        rtt_count += 1;
                        last_responder = Some(result.responder_ip.to_string());
                        
                        if is_dest {
                            is_dest_reached = true;
                        }
                    }
                    Err(ProbeError::IcmpTimeout) => {
                        tracing::warn!("Timeout/Error for TTL={}, Probe={}: {}", ttl, probe_idx, ProbeError::IcmpTimeout);
                    }
                    Err(e) => {
                        tracing::error!("Fatal Error for TTL={}: {}", ttl, e);
                        fatal_error = Some(e);
                        break;
                    }
                }
                
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            
            if let Some(err) = fatal_error {
                tracing::error!("Traceroute probe failing due to fatal error: {}", err);
                return Err(err);
            }
            
            let hop = if rtt_count > 0 {
                TracerouteHop {
                    hop_number: ttl,
                    ip_address: last_responder,
                    avg_rtt_ms: Some(rtt_sum / rtt_count as f64),
                    is_destination: is_dest_reached,
                }
            } else {
                TracerouteHop {
                    hop_number: ttl,
                    ip_address: None,
                    avg_rtt_ms: None,
                    is_destination: false,
                }
            };
            
            if let Err(e) = tx.try_send(TelemetryEvent::TracerouteHop(hop)) {
                match e {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                        // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                        tracing::warn!(target: "probe_telemetry", "Telemetry channel full, shedding load / dropping probe frame");
                    }
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        return Ok(());
                    }
                }
            }

            if is_dest_reached {
                break;
            }
        }

        if let Err(e) = tx.try_send(TelemetryEvent::TracerouteComplete) {
            match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                    tracing::warn!(target: "probe_telemetry", "Telemetry channel full, shedding load / dropping probe frame");
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
