use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::ProbeError;
use super::{NetworkProbe, TelemetryEvent};

pub struct TcpProbe {
    pub target_ip: Arc<String>,
    pub resolved_addr: SocketAddr,
    pub interval: Duration,
    pub timeout: Duration,
}

impl TcpProbe {
    pub fn new(target_ip: Arc<String>, resolved_addr: SocketAddr, interval: Duration, timeout: Duration) -> Self {
        Self { target_ip, resolved_addr, interval, timeout }
    }
}

impl NetworkProbe for TcpProbe {
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
                    let target_addr = self.resolved_addr;
                    let timeout_duration = self.timeout;
                    let tx_clone = tx.clone();
                    let target_ip_clone = Arc::clone(&self.target_ip);

                    tokio::spawn(async move {
                        let start_time = Instant::now();
                        
                        let tcp_ping_result = match tokio::time::timeout(
                            timeout_duration, 
                            TcpStream::connect(target_addr)
                        ).await {
                            Ok(Ok(stream)) => {
                                let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
                                
                                tokio::task::spawn_blocking(move || {
                                    let sock_ref = socket2::SockRef::from(&stream);
                                    if let Err(e) = sock_ref.set_linger(Some(Duration::from_secs(0))) {
                                        tracing::debug!("Failed to set linger (ignoring): {}", e);
                                    }
                                    drop(stream);
                                });

                                Ok(elapsed)
                            },
                            Ok(Err(e)) => Err(ProbeError::Socket(e)),
                            Err(_) => Err(ProbeError::TcpTimeout),
                        };

                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or(Duration::from_secs(0))
                            .as_secs();

                        let event = TelemetryEvent::Tcp {
                            sequence_number: current_seq,
                            target_ip: target_ip_clone.to_string(),
                            result: tcp_ping_result,
                            timestamp,
                        };

                        if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx_clone.try_send(event) {
                            return;
                        }
                    });
                }
            }
        }
        Ok(())
    }
}
