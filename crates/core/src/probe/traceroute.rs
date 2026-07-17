use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::{ProbeError, TracerouteHop};
use crate::network::icmp::{TracerouteIcmpProvider, IcmpProvider};
use super::{NetworkProbe, TelemetryEvent};
use crate::network::icmp::packet::IcmpResponse;

pub struct TracerouteProbe {
    pub resolved_addr: SocketAddr,
    pub timeout: Duration,
    pub max_hops: u8,
    pub icmp_identifier: u16,
}

impl TracerouteProbe {
    pub fn new(resolved_addr: SocketAddr, timeout: Duration, max_hops: u8, icmp_identifier: u16) -> Self {
        Self { resolved_addr, timeout, max_hops, icmp_identifier }
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
            let mut last_icmp_err = None;
            
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
                        let (is_dest, icmp_error_msg) = match result.response {
                            IcmpResponse::EchoReply(r) => {
                                let _t = r.type_; let _c = r.code;
                                (true, None)
                            },
                            IcmpResponse::TimeExceeded(t) => {
                                (false, Some(format!("Type: 11, Code: {}", t.code)))
                            },
                            IcmpResponse::DestinationUnreachable(d) => {
                                (true, Some(format!("Type: 3, Code: {}", d.code)))
                            },
                            IcmpResponse::Unknown { type_, code } => {
                                (false, Some(format!("Type: {}, Code: {}", type_, code)))
                            },
                        };
                        
                        rtt_sum += result.rtt_ms;
                        rtt_count += 1;
                        last_responder = Some(result.responder_ip.to_string());
                        
                        if is_dest {
                            is_dest_reached = true;
                        }
                        
                        if icmp_error_msg.is_some() {
                            last_icmp_err = icmp_error_msg;
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
                    icmp_error: last_icmp_err,
                }
            } else {
                TracerouteHop {
                    hop_number: ttl,
                    ip_address: None,
                    avg_rtt_ms: None,
                    is_destination: false,
                    icmp_error: None,
                }
            };
            
            if let Err(e) = tx.try_send(TelemetryEvent::TracerouteHop(hop.clone())) {
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

            if let Some(ip) = hop.ip_address {
                let tx_dns = tx.clone();
                tokio::spawn(async move {
                    let ip_clone = ip.clone();
                    let hostname = tokio::task::spawn_blocking(move || {
                        match ip_clone.parse::<std::net::IpAddr>() {
                            Ok(addr) => dns_lookup::lookup_addr(&addr).ok(),
                            Err(_) => None,
                        }
                    }).await.unwrap_or(None);
                    
                    let _ = tx_dns.send(TelemetryEvent::DnsResolved {
                        ip,
                        hostname,
                    }).await;
                });
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
