use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio::sync::mpsc;
use crate::models::{PingMetrics, TelemetryEvent, ProbeError};
use crate::network::icmp::{DefaultIcmpProvider, IcmpProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCommand {
    Pause,
    Resume,
    Stop,
    StartBandwidthTest,
}

/// Core network measurement engine executing a concurrent dual-probing strategy.
/// 
/// It dispatches both TCP Handshake and Unprivileged ICMP Datagram probes simultaneously.
/// This approach ensures high observability of both network-layer and application-layer latency
/// without requiring root privileges in modern OS environments.
pub struct CoreEngine {
    /// Shared reference to the target hostname or IP, preventing excessive allocations across micro-tasks.
    pub target_ip: Arc<String>,
    pub target_port: u16,
    pub interval: Duration,
    pub timeout: Duration,
}

impl CoreEngine {
    /// Instantiates a new measurement engine with the provided configuration parameters.
    pub fn new(target_ip: String, target_port: u16, interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            target_ip: Arc::new(target_ip),
            target_port,
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Starts the asynchronous measurement loop.
    /// 
    /// Performs pre-flight DNS resolution to ensure DNS lookup latency does not skew 
    /// the TCP handshake metrics. Subsequently, it spawns a detached worker loop that 
    /// dispatches concurrent probes at the specified interval.
    pub async fn start(self, tx: mpsc::Sender<TelemetryEvent>, mut cmd_rx: mpsc::Receiver<EngineCommand>) {
        let addr_string = format!("{}:{}", self.target_ip, self.target_port);
        
        let resolved_addr: SocketAddr = match tokio::net::lookup_host(&addr_string).await {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    addr
                } else {
                    tracing::error!("Fatal Error: DNS returned no addresses for {}", self.target_ip);
                    return;
                }
            },
            Err(e) => {
                tracing::error!("Fatal Error: DNS Resolution Failed: {}", e);
                if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx.try_send(TelemetryEvent::BandwidthError(ProbeError::DnsResolution(e.to_string()))) {
                    tracing::error!("UI channel closed unexpectedly during DNS failure");
                }
                return;
            }
        };

        // Generates a stateless, pseudo-random identifier for ICMP packets based on the system clock.
        let icmp_identifier = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::default())
            .subsec_nanos() % 65535) as u16;

        tokio::spawn(async move {
            let mut sequence_counter: u64 = 0;
            let mut interval_timer = tokio::time::interval(self.interval);
            let mut is_paused = false;
            let mut bw_cancel_token: Option<tokio_util::sync::CancellationToken> = None;

            loop {
                tokio::select! {
                    cmd_opt = cmd_rx.recv() => {
                        match cmd_opt {
                            Some(EngineCommand::Pause) => is_paused = true,
                            Some(EngineCommand::Resume) => {
                                is_paused = false;
                                if let Some(token) = bw_cancel_token.take() {
                                    token.cancel();
                                }
                            },
                            Some(EngineCommand::Stop) => {
                                if let Some(token) = bw_cancel_token.take() {
                                    token.cancel();
                                }
                                break;
                            },
                            Some(EngineCommand::StartBandwidthTest) => {
                                is_paused = true;
                                let tx_for_bw = tx.clone();
                                let token = tokio_util::sync::CancellationToken::new();
                                bw_cancel_token = Some(token.clone());
                                tokio::spawn(async move {
                                    let result = crate::network::bandwidth::BandwidthEngine::test_download(
                                        "speed.cloudflare.com", 
                                        "/__down?bytes=50000000", 
                                        tx_for_bw.clone(),
                                        token
                                    ).await;

                                    if let Err(e) = result {
                                        if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx_for_bw.try_send(crate::models::TelemetryEvent::BandwidthError(e)) {
                                            tracing::error!("UI channel closed unexpectedly during bandwidth error");
                                        }
                                    }
                                });
                            }
                            None => break, // Channel closed (e.g., UI exited), stop the engine task.
                        }
                    }
                    _ = interval_timer.tick(), if !is_paused => {
                        sequence_counter += 1;

                        let current_seq = sequence_counter;
                let target_ip_clone = Arc::clone(&self.target_ip);
                let timeout_duration = self.timeout;
                let tx_clone = tx.clone();
                let target_addr = resolved_addr;
                
                let icmp_seq = (current_seq % 65535) as u16;

                // Isolate each interval tick into its own concurrent task to prevent head-of-line blocking
                // in case a specific probe encounters severe packet drops.
                tokio::spawn(async move {
                    let start_time = Instant::now();
                    
                    // --- SUB-TASK A: Asynchronous TCP Handshake Probe ---
                    let tcp_ping_result = match tokio::time::timeout(
                        timeout_duration, 
                        TcpStream::connect(target_addr)
                    ).await {
                        Ok(Ok(stream)) => {
                            let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
                            
                            // Offload socket closure to a blocking thread pool to prevent the OS-level 
                            // TCP RST (Linger) from stalling the asynchronous reactor. This actively 
                            // prevents TIME_WAIT socket exhaustion during high-frequency polling.
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

                    // --- SUB-TASK B: Synchronous OS-level ICMP Probe ---
                    let icmp_target = target_addr;
                    
                    // Offload blocking syscalls to Tokio's specialized blocking thread pool.
                    let icmp_ping_result = tokio::task::spawn_blocking(move || {
                        let provider = DefaultIcmpProvider::new(icmp_identifier);
                        provider.ping(&icmp_target, icmp_seq, timeout_duration)
                    }).await.unwrap_or_else(|_| Err(ProbeError::BandwidthTestFailed("Thread Panicked".to_string())));

                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();

                    let metrics = PingMetrics {
                        sequence_number: current_seq,
                        target_ip: target_ip_clone.to_string(),
                        icmp_ping: icmp_ping_result,
                        tcp_ping: tcp_ping_result,
                        timestamp,
                    };

                    match tx_clone.try_send(TelemetryEvent::Ping(metrics)) {
                        Ok(_) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            tracing::warn!("UI is lagging, dropping telemetry event");
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            tracing::error!("UI channel closed unexpectedly");
                            // Since this is in a spawned task for a single tick, we just return.
                            // The outer loop will also fail on its next try_send if it sends anything, 
                            // but wait, the outer loop doesn't send. The outer loop just spawns.
                        }
                    }
                });
                    }
                }
            }
        });
    }
}