use crate::models::ProbeError;
use crate::probe::{NetworkProbe, TelemetryEvent, ping::PingProbe, tcp::TcpProbe};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
    async fn start(
        self,
        tx: mpsc::Sender<TelemetryEvent>,
        mut cmd_rx: mpsc::Receiver<EngineCommand>,
    ) {
        let addr_string = format!("{}:{}", self.target_ip, self.target_port);
        let resolved_addr: SocketAddr = match tokio::net::lookup_host(&addr_string).await {
            Ok(addrs) => {
                if let Some(addr) = crate::network::pick_address(addrs) {
                    addr
                } else {
                    tracing::error!(
                        "DNS resolution succeeded but returned zero IP addresses for the target."
                    );
                    let err =
                        ProbeError::DnsResolution("No IP addresses found for host".to_string());
                    if tx.send(TelemetryEvent::Fatal(err)).await.is_err() {
                        tracing::error!("UI channel is dead. Unable to propagate fatal DNS error.");
                    }
                    return;
                }
            }
            Err(e) => {
                let probe_err = match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        tracing::error!("Fatal Error: DNS Resolution Failed: Host not found");
                        ProbeError::DnsResolution("Host not found".to_string())
                    }
                    std::io::ErrorKind::TimedOut => {
                        tracing::error!(
                            "Fatal Error: DNS Resolution Failed: DNS request timed out"
                        );
                        ProbeError::DnsResolution("DNS request timed out".to_string())
                    }
                    _ => {
                        tracing::error!("Fatal Error: DNS Resolution Failed: {}", e);
                        ProbeError::Socket(e)
                    }
                };
                if let Err(send_err) = tx.try_send(TelemetryEvent::Fatal(probe_err)) {
                    match send_err {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                            tracing::warn!(
                                "UI channel overloaded, dropping telemetry frame to prevent memory exhaustion"
                            );
                        }
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                            tracing::error!(
                                "UI channel is dead. Shutting down due to DNS failure."
                            );
                            return;
                        }
                    }
                }
                return;
            }
        };

        let icmp_identifier = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => (d.subsec_nanos() % 65535) as u16,
            Err(e) => {
                tracing::error!("System Time Error: {}", e);
                if tx
                    .send(TelemetryEvent::Fatal(ProbeError::TimeSyncError))
                    .await
                    .is_err()
                {
                    tracing::error!(
                        "UI channel is dead. Unable to propagate fatal time synchronization error."
                    );
                    return;
                }
                return;
            }
        };

        tokio::spawn(async move {
            let token = CancellationToken::new();
            let mut active_token: Option<CancellationToken> = Some(token.clone());
            let mut bw_cancel_token: Option<CancellationToken> = None;
            let mut trace_cancel_token: Option<CancellationToken> = None;
            Self::spawn_probes(
                self.target_ip.clone(),
                resolved_addr,
                self.interval,
                self.timeout,
                icmp_identifier,
                tx.clone(),
                token,
            );

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    EngineCommand::Pause => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }
                    }
                    EngineCommand::Resume => {
                        if let Some(token) = bw_cancel_token.take() {
                            token.cancel();
                        }
                        if let Some(token) = trace_cancel_token.take() {
                            token.cancel();
                        }
                        if active_token.is_none() {
                            let new_token = CancellationToken::new();
                            active_token = Some(new_token.clone());
                            Self::spawn_probes(
                                self.target_ip.clone(),
                                resolved_addr,
                                self.interval,
                                self.timeout,
                                icmp_identifier,
                                tx.clone(),
                                new_token,
                            );
                        }
                    }
                    EngineCommand::Stop => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }
                        if let Some(token) = bw_cancel_token.take() {
                            token.cancel();
                        }
                        if let Some(token) = trace_cancel_token.take() {
                            token.cancel();
                        }
                        break;
                    }
                    EngineCommand::StartBandwidthTest => {
                        if let Some(token) = active_token.take() {
                            token.cancel();
                        }

                        let tx_for_bw = tx.clone();
                        let token = CancellationToken::new();
                        bw_cancel_token = Some(token.clone());
                        tokio::spawn(async move {
                            let result = crate::network::bandwidth::test_download(
                                "speed.cloudflare.com",
                                "/__down?bytes=50000000",
                                tx_for_bw.clone(),
                                token,
                            )
                            .await;

                            if let Err(e) = result {
                                tracing::error!("Bandwidth test execution failed: {:?}", e);
                                if let Err(send_err) =
                                    tx_for_bw.try_send(TelemetryEvent::BandwidthError(e))
                                {
                                    match send_err {
                                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                            tracing::warn!(
                                                "UI channel overloaded, dropping telemetry frame to prevent memory exhaustion"
                                            );
                                        }
                                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                            tracing::error!(
                                                "UI channel is dead. Shutting down bandwidth worker loop."
                                            );
                                        }
                                    }
                                }
                            }
                        });
                    }
                    EngineCommand::StartTraceroute(_target) => {
                        if let Some(token) = active_token.take() {
                            token.cancel(); // Pause ping/tcp while tracerouting
                        }

                        if let Some(token) = trace_cancel_token.take() {
                            token.cancel();
                        }
                        let token = CancellationToken::new();
                        trace_cancel_token = Some(token.clone());
                        Self::spawn_traceroute(resolved_addr, icmp_identifier, tx.clone(), token);
                    }
                }
            }
        });
    }
}

impl CoreEngine {
    fn spawn_probes(
        target_ip: Arc<String>,
        resolved_addr: SocketAddr,
        interval: Duration,
        timeout: Duration,
        icmp_identifier: u16,
        tx: mpsc::Sender<TelemetryEvent>,
        token: CancellationToken,
    ) {
        let mut ping_probe = PingProbe::new(
            target_ip.clone(),
            resolved_addr,
            interval,
            timeout,
            icmp_identifier,
        );
        let mut tcp_probe = TcpProbe::new(target_ip, resolved_addr, interval, timeout);

        let tx_ping = tx.clone();
        let token_ping = token.clone();
        tokio::spawn(async move {
            if let Err(e) = ping_probe.run(tx_ping.clone(), token_ping).await {
                tracing::error!("Ping probe encountered fatal error: {:?}", e);
                if tx_ping.send(TelemetryEvent::Fatal(e)).await.is_err() {
                    tracing::error!(
                        "UI channel is dead. Cannot send fatal telemetry event. Shutting down engine worker."
                    );
                }
            }
        });

        let tx_tcp = tx.clone();
        let token_tcp = token.clone();
        tokio::spawn(async move {
            if let Err(e) = tcp_probe.run(tx_tcp.clone(), token_tcp).await {
                tracing::error!("TCP probe encountered fatal error: {:?}", e);
                if tx_tcp.send(TelemetryEvent::Fatal(e)).await.is_err() {
                    tracing::error!(
                        "UI channel is dead. Cannot send fatal telemetry event. Shutting down engine worker."
                    );
                }
            }
        });
    }

    /// Runs a traceroute in this process. Hops are forwarded to the UI, each responder
    /// gets a reverse-DNS lookup, and probe failures are reported as `TracerouteError`
    /// so the traceroute screen shows them instead of the fatal error page.
    fn spawn_traceroute(
        resolved_addr: SocketAddr,
        icmp_identifier: u16,
        tx: mpsc::Sender<TelemetryEvent>,
        token: CancellationToken,
    ) {
        let (probe_tx, mut probe_rx) = mpsc::channel(100);
        let probe_token = token.clone();
        let probe_task = tokio::spawn(async move {
            let mut probe = crate::probe::traceroute::TracerouteProbe::new(
                resolved_addr,
                Duration::from_millis(1000),
                30,
                icmp_identifier,
            );
            probe.run(probe_tx, probe_token).await
        });

        tokio::spawn(async move {
            while let Some(event) = probe_rx.recv().await {
                // A cancelled run must not leak hops into the next screen.
                if token.is_cancelled() {
                    return;
                }
                if let TelemetryEvent::TracerouteHop(hop) = &event
                    && let Some(ip) = hop.ip_address.clone()
                {
                    let tx_dns = tx.clone();
                    tokio::spawn(async move {
                        let hostname = reverse_dns(&ip).await;
                        let _ = tx_dns
                            .send(TelemetryEvent::DnsResolved { ip, hostname })
                            .await;
                    });
                }
                if tx.send(event).await.is_err() {
                    return;
                }
            }

            let error = match probe_task.await {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(e) => ProbeError::ThreadPanic(format!("Traceroute task panicked: {}", e)),
            };
            tracing::error!("Traceroute probe failed: {}", error);
            if !token.is_cancelled() {
                let _ = tx.send(TelemetryEvent::TracerouteError(error)).await;
            }
        });
    }
}

async fn reverse_dns(ip: &str) -> Option<String> {
    let addr: std::net::IpAddr = ip.parse().ok()?;
    tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&addr).ok())
        .await
        .ok()
        .flatten()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::engine::NetworkEngine;

    /// Loopback only and no admin: the ICMP Helper API answers 127.0.0.1 in-process.
    #[tokio::test]
    async fn traceroute_to_loopback_runs_in_process() {
        let (tx, mut rx) = mpsc::channel(100);
        let (cmd_tx, cmd_rx) = mpsc::channel(10);
        CoreEngine::new("127.0.0.1".to_string(), 9, 500, 500)
            .start(tx, cmd_rx)
            .await;
        cmd_tx.send(EngineCommand::Pause).await.unwrap();
        cmd_tx
            .send(EngineCommand::StartTraceroute("127.0.0.1".to_string()))
            .await
            .unwrap();

        let mut hops = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(TelemetryEvent::TracerouteHop(hop))) => hops.push(hop),
                Ok(Some(TelemetryEvent::TracerouteComplete)) => break,
                Ok(Some(TelemetryEvent::TracerouteError(e))) => panic!("traceroute failed: {e}"),
                Ok(Some(TelemetryEvent::Fatal(e))) => panic!("fatal: {e}"),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => panic!("traceroute did not complete; hops: {hops:?}"),
            }
        }
        assert_eq!(hops.len(), 1);
        assert!(hops[0].is_destination);
        assert_eq!(hops[0].ip_address.as_deref(), Some("127.0.0.1"));
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
    }
}
