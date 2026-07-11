use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::models::ProbeError;
use crate::probe::{NetworkProbe, TelemetryEvent, ping::PingProbe, tcp::TcpProbe};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    Pause,
    Resume,
    Stop,
    StartBandwidthTest,
    StartTraceroute(String),
}

pub struct CoreEngine {
    pub target_ip: Arc<String>,
    pub target_port: u16,
    pub interval: Duration,
    pub timeout: Duration,
}

impl CoreEngine {
    pub fn new(target_ip: String, target_port: u16, interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            target_ip: Arc::new(target_ip),
            target_port,
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

impl super::NetworkEngine for CoreEngine {
    fn start(self: Box<Self>, tx: mpsc::Sender<TelemetryEvent>, mut cmd_rx: mpsc::Receiver<EngineCommand>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
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
                if let Err(send_err) = tx.try_send(TelemetryEvent::BandwidthError(ProbeError::DnsResolution(e.to_string()))) {
                    match send_err {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                            // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                        }
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                            return;
                        }
                    }
                }
                return;
            }
        };

        let icmp_identifier = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => (d.subsec_nanos() % 65535) as u16,
            Err(_) => {
                if tx.send(TelemetryEvent::Fatal(ProbeError::TimeSyncError)).await.is_err() {
                    return;
                }
                return;
            }
        };

        tokio::spawn(async move {
            let token = CancellationToken::new();
            let mut active_token: Option<CancellationToken> = Some(token.clone());
            let mut bw_cancel_token: Option<CancellationToken> = None;
            Self::spawn_probes(&self.target_ip, resolved_addr, self.interval, self.timeout, icmp_identifier, tx.clone(), token);

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    EngineCommand::Pause => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }
                    },
                    EngineCommand::Resume => {
                        if let Some(token) = bw_cancel_token.take() {
                            token.cancel();
                        }
                        if active_token.is_none() {
                            let new_token = CancellationToken::new();
                            active_token = Some(new_token.clone());
                            Self::spawn_probes(&self.target_ip, resolved_addr, self.interval, self.timeout, icmp_identifier, tx.clone(), new_token);
                        }
                    },
                    EngineCommand::Stop => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }
                        if let Some(token) = bw_cancel_token.take() {
                            token.cancel();
                        }
                        break;
                    },
                    EngineCommand::StartBandwidthTest => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }
                        
                        let tx_for_bw = tx.clone();
                        let token = CancellationToken::new();
                        bw_cancel_token = Some(token.clone());
                        tokio::spawn(async move {
                            let result = crate::network::bandwidth::BandwidthEngine::test_download(
                                "speed.cloudflare.com", 
                                "/__down?bytes=50000000", 
                                tx_for_bw.clone(),
                                token
                            ).await;

                            if let Err(e) = result
                                && let Err(send_err) = tx_for_bw.try_send(TelemetryEvent::BandwidthError(e)) {
                                    match send_err {
                                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                            // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                                        }
                                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                        }
                                    }
                                }
                        });
                    },
                    EngineCommand::StartTraceroute(target) => {
                        if let Some(token) = active_token.take() {
                            token.cancel(); // Pause ping/tcp while tracerouting
                        }

                        let tx_traceroute = tx.clone();
                        let token = CancellationToken::new();
                        let identifier = icmp_identifier;
                        let addr = resolved_addr;
                        
                        tokio::spawn(async move {
                            let mut probe = crate::probe::traceroute::TracerouteProbe::new(
                                Arc::new(target), 
                                addr, 
                                Duration::from_millis(1000), 
                                30, 
                                identifier
                            );
                            
                            if let Err(e) = crate::probe::NetworkProbe::run(&mut probe, tx_traceroute.clone(), token).await {
                                tracing::error!("Traceroute probe encountered fatal error: {:?}", e);
                                if tx_traceroute.send(TelemetryEvent::Fatal(e)).await.is_err() {
                                }
                            }
                        });
                    }
                }
            }
        });
        })
    }
}

impl CoreEngine {
    fn spawn_probes(
        target_ip: &Arc<String>,
        resolved_addr: SocketAddr,
        interval: Duration,
        timeout: Duration,
        icmp_identifier: u16,
        tx: mpsc::Sender<TelemetryEvent>,
        token: CancellationToken,
    ) {
        let mut ping_probe = PingProbe::new(Arc::clone(target_ip), resolved_addr, interval, timeout, icmp_identifier);
        let mut tcp_probe = TcpProbe::new(Arc::clone(target_ip), resolved_addr, interval, timeout);

        let tx_ping = tx.clone();
        let token_ping = token.clone();
        tokio::spawn(async move {
            if let Err(e) = ping_probe.run(tx_ping.clone(), token_ping).await {
                tracing::error!("Ping probe encountered fatal error: {:?}", e);
                if tx_ping.send(TelemetryEvent::Fatal(e)).await.is_err() {
                }
            }
        });

        let tx_tcp = tx.clone();
        let token_tcp = token.clone();
        tokio::spawn(async move {
            if let Err(e) = tcp_probe.run(tx_tcp.clone(), token_tcp).await {
                tracing::error!("TCP probe encountered fatal error: {:?}", e);
                if tx_tcp.send(TelemetryEvent::Fatal(e)).await.is_err() {
                }
            }
        });
    }
}