use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::ProbeError;
use crate::network::icmp::{DefaultIcmpProvider, IcmpProvider};
use super::{NetworkProbe, TelemetryEvent};

pub struct PingProbe {
    pub target_ip: Arc<String>,
    pub resolved_addr: SocketAddr,
    pub interval: Duration,
    pub timeout: Duration,
    pub icmp_identifier: u16,
}

impl PingProbe {
    pub fn new(target_ip: Arc<String>, resolved_addr: SocketAddr, interval: Duration, timeout: Duration, icmp_identifier: u16) -> Self {
        Self { target_ip, resolved_addr, interval, timeout, icmp_identifier }
    }
}

impl NetworkProbe for PingProbe {
    async fn run(&mut self, tx: mpsc::Sender<TelemetryEvent>, cancel_token: CancellationToken) -> Result<(), ProbeError> {
        let mut sequence_counter: u64 = 0;
        let mut interval_timer = tokio::time::interval(self.interval);

        loop {
            if tx.is_closed() {
                break;
            }
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }
                _ = interval_timer.tick() => {
                    sequence_counter += 1;
                    let current_seq = sequence_counter;
                    let icmp_seq = (current_seq % 65535) as u16;
                    let target_addr = self.resolved_addr;
                    let timeout_duration = self.timeout;
                    let identifier = self.icmp_identifier;

                    let icmp_ping_result = {
                        let provider = DefaultIcmpProvider::new(identifier);
                        provider.ping(&target_addr, icmp_seq, timeout_duration).await
                    };

                    let timestamp = match crate::utils::current_timestamp() {
                        Ok(ts) => ts,
                        Err(e) => {
                            tracing::error!("Failed to fetch current timestamp: {:?}", e);
                            return Err(e);
                        }
                    };

                    let event = TelemetryEvent::Ping {
                        sequence_number: current_seq,
                        target_ip: self.target_ip.to_string(),
                        result: icmp_ping_result,
                        timestamp,
                    };

                    if let Err(e) = tx.try_send(event) {
                        match e {
                            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                            }
                            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
