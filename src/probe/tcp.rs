use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
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

                    let start_time = Instant::now();
                    
                    let tcp_ping_result = match tokio::time::timeout(
                        timeout_duration, 
                        TcpStream::connect(target_addr)
                    ).await {
                        Ok(Ok(stream)) => {
                            let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
                            
                            if let Err(e) = tokio::task::spawn_blocking(move || {
                                let sock_ref = socket2::SockRef::from(&stream);
                                if let Err(e) = sock_ref.set_linger(Some(Duration::from_secs(0))) {
                                    tracing::debug!("Failed to set linger (ignoring): {}", e);
                                }
                                drop(stream);
                            }).await {
                                tracing::error!("Thread Panic: {}", e);
                                return Err(ProbeError::ThreadPanic(e.to_string()));
                            }

                            Ok(elapsed)
                        },
                        Ok(Err(e)) => {
                            match e.kind() {
                                std::io::ErrorKind::ConnectionRefused => {
                                    tracing::warn!("TCP Connection Refused: {}", e);
                                    Err(ProbeError::Socket(e))
                                }
                                std::io::ErrorKind::TimedOut => {
                                    tracing::warn!("TCP Connection TimedOut: {}", e);
                                    Err(ProbeError::TcpTimeout)
                                }
                                std::io::ErrorKind::PermissionDenied => {
                                    tracing::warn!("TCP Permission Denied: {}", e);
                                    Err(ProbeError::PermissionDenied)
                                }
                                _ => {
                                    tracing::error!("TCP I/O Error: {}", e);
                                    Err(ProbeError::Socket(e))
                                }
                            }
                        },
                        Err(e) => {
                            tracing::warn!("Timeout: {}", e);
                            Err(ProbeError::TcpTimeout)
                        },
                    };

                    let timestamp = match crate::utils::current_timestamp() {
                        Ok(ts) => ts,
                        Err(e) => {
                            tracing::error!("Failed to fetch current timestamp: {:?}", e);
                            return Err(e);
                        }
                    };

                    let event = TelemetryEvent::Tcp {
                        sequence_number: current_seq,
                        target_ip: self.target_ip.to_string(),
                        result: tcp_ping_result,
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
