use super::NetworkEngine;
use super::core_engine::EngineCommand;
use crate::models::{ProbeError, TracerouteHop};
use crate::probe::TelemetryEvent;
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MOCK_TARGET: &str = "mock.local";

/// TEST-NET-1 (RFC 5737): never routed, so mock output can never be mistaken for a real host.
pub(crate) const MOCK_IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);

/// The mock traceroute, shared by `MockEngine` and `measure`'s `Source::Mock`:
/// `(ip, hostname, rtt_ms)`; the last hop is `MOCK_IP`, the destination.
pub(crate) const MOCK_HOPS: [(Ipv4Addr, &str, f64); 3] = [
    (Ipv4Addr::new(192, 0, 2, 254), "gateway.mock", 1.0),
    (Ipv4Addr::new(192, 0, 2, 100), "isp.mock", 5.0),
    (MOCK_IP, "target.mock", 10.0),
];

/// The probe-side hop for `MOCK_HOPS[index]` (hop numbers start at 1).
pub(crate) fn mock_hop(index: usize, ip: Ipv4Addr, rtt_ms: f64) -> TracerouteHop {
    TracerouteHop {
        hop_number: (index + 1) as u8,
        ip_address: Some(ip.to_string()),
        avg_rtt_ms: Some(rtt_ms),
        is_destination: ip == MOCK_IP,
        icmp_error: None,
    }
}

/// Pause between simulated hops, so the traceroute screen fills in visibly.
const MOCK_HOP_DELAY: Duration = Duration::from_millis(300);

/// Simulated engine that emits deterministic ping/TCP telemetry without touching the network.
/// Used by `--mock` and by tests that need a live event stream.
pub struct MockEngine {
    pub interval: Duration,
}

impl Default for MockEngine {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(500),
        }
    }
}

/// Deterministic latency pattern: a sawtooth around 20 ms, with every 20th ICMP probe lost.
/// Shared by `MockEngine` (TUI `--mock`) and `measure`'s `Source::Mock`, so both show the same
/// numbers. Returns `(icmp, tcp)`.
pub fn sample(sequence: u64) -> (Result<f64, ProbeError>, Result<f64, ProbeError>) {
    let icmp_ms = 20.0 + ((sequence * 7) % 13) as f64;
    let icmp = if sequence.is_multiple_of(20) {
        Err(ProbeError::IcmpTimeout)
    } else {
        Ok(icmp_ms)
    };
    (icmp, Ok(icmp_ms + 5.0))
}

impl NetworkEngine for MockEngine {
    async fn start(
        self,
        tx: mpsc::Sender<TelemetryEvent>,
        mut cmd_rx: mpsc::Receiver<EngineCommand>,
    ) {
        tracing::info!("MockEngine started. Simulating telemetry...");

        // Run in the background like CoreEngine, so the caller can start the UI right away.
        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(self.interval);
            let mut sequence: u64 = 0;
            let mut paused = false;
            // The running simulated traceroute; cancelled when the user leaves its screen.
            let mut trace_token: Option<CancellationToken> = None;

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(EngineCommand::Stop) | None => {
                                if let Some(token) = trace_token.take() {
                                    token.cancel();
                                }
                                break;
                            }
                            Some(EngineCommand::Resume) => {
                                if let Some(token) = trace_token.take() {
                                    token.cancel();
                                }
                                paused = false;
                            }
                            Some(EngineCommand::StartTraceroute(_)) => {
                                // Pings stay paused while the traceroute screen is open, as in
                                // CoreEngine. Without this the TUI waited forever on `t`.
                                paused = true;
                                if let Some(token) = trace_token.take() {
                                    token.cancel();
                                }
                                let token = CancellationToken::new();
                                trace_token = Some(token.clone());
                                tokio::spawn(simulate_traceroute(tx.clone(), token));
                            }
                            Some(cmd) => {
                                // Pause, and the bandwidth test, which is not simulated.
                                tracing::info!("MockEngine pausing on command: {:?}", cmd);
                                paused = true;
                            }
                        }
                    }
                    _ = interval_timer.tick(), if !paused => {
                        sequence += 1;
                        let (icmp, tcp) = sample(sequence);
                        let timestamp = crate::utils::current_timestamp().unwrap_or_default();

                        let events = [
                            TelemetryEvent::Ping {
                                sequence_number: sequence,
                                target_ip: MOCK_TARGET.to_string(),
                                result: icmp,
                                timestamp,
                            },
                            TelemetryEvent::Tcp {
                                sequence_number: sequence,
                                target_ip: MOCK_TARGET.to_string(),
                                result: tcp,
                                timestamp,
                            },
                        ];
                        for event in events {
                            if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(event) {
                                tracing::info!("MockEngine stopped: UI channel closed.");
                                return;
                            }
                        }
                    }
                }
            }
            tracing::info!("MockEngine stopped.");
        });
    }
}

/// Sends the `MOCK_HOPS` with their hostnames, then `TracerouteComplete`, unless cancelled.
async fn simulate_traceroute(tx: mpsc::Sender<TelemetryEvent>, cancel: CancellationToken) {
    for (i, (ip, hostname, rtt)) in MOCK_HOPS.iter().enumerate() {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(MOCK_HOP_DELAY) => {}
        }
        let events = [
            TelemetryEvent::TracerouteHop(mock_hop(i, *ip, *rtt)),
            TelemetryEvent::DnsResolved {
                ip: ip.to_string(),
                hostname: Some((*hostname).to_string()),
            },
        ];
        for event in events {
            match tx.try_send(event) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("Mock traceroute: telemetry channel full, dropping frame");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    }
    if cancel.is_cancelled() {
        return;
    }
    if let Err(e) = tx.try_send(TelemetryEvent::TracerouteComplete) {
        tracing::warn!("Mock traceroute: could not send completion: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TracerouteHop;

    /// Events until `TracerouteComplete` (inclusive); panics if it never comes.
    async fn until_complete(rx: &mut mpsc::Receiver<TelemetryEvent>) -> Vec<TelemetryEvent> {
        let mut events = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
                Ok(Some(TelemetryEvent::TracerouteComplete)) => {
                    events.push(TelemetryEvent::TracerouteComplete);
                    return events;
                }
                Ok(Some(e)) => events.push(e),
                other => panic!("traceroute never completed: {other:?}, got {events:?}"),
            }
        }
    }

    fn hops(events: &[TelemetryEvent]) -> Vec<&TracerouteHop> {
        events
            .iter()
            .filter_map(|e| match e {
                TelemetryEvent::TracerouteHop(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    fn is_latency(e: &TelemetryEvent) -> bool {
        matches!(e, TelemetryEvent::Ping { .. } | TelemetryEvent::Tcp { .. })
    }

    /// Starts the mock and sends what the TUI sends on `t`: Pause, then StartTraceroute.
    async fn start_traceroute() -> (mpsc::Receiver<TelemetryEvent>, mpsc::Sender<EngineCommand>) {
        let (tx, rx) = mpsc::channel(64);
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        MockEngine::default().start(tx, cmd_rx).await;
        cmd_tx.send(EngineCommand::Pause).await.unwrap();
        cmd_tx
            .send(EngineCommand::StartTraceroute(MOCK_TARGET.to_string()))
            .await
            .unwrap();
        (rx, cmd_tx)
    }

    #[tokio::test(start_paused = true)]
    async fn start_traceroute_emits_hops_dns_and_complete() {
        let (mut rx, cmd_tx) = start_traceroute().await;
        let events = until_complete(&mut rx).await;

        let hops = hops(&events);
        let numbers: Vec<u8> = hops.iter().map(|h| h.hop_number).collect();
        assert_eq!(numbers, [1, 2, 3]);
        assert!(hops[2].is_destination);
        assert!(hops[..2].iter().all(|h| !h.is_destination));
        assert_eq!(hops[2].ip_address.as_deref(), Some("192.0.2.1"));

        for (hop, name) in hops.iter().zip(["gateway.mock", "isp.mock", "target.mock"]) {
            let ip = hop.ip_address.clone().unwrap();
            let named = events.iter().any(|e| {
                matches!(e, TelemetryEvent::DnsResolved { ip: i, hostname: Some(h) }
                    if *i == ip && h == name)
            });
            assert!(named, "{ip} should resolve to {name}");
        }

        // Pings stay paused while the traceroute screen is open.
        let first_hop = events
            .iter()
            .position(|e| matches!(e, TelemetryEvent::TracerouteHop(_)))
            .unwrap();
        assert!(!events[first_hop..].iter().any(is_latency));
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn resume_after_traceroute_restarts_pings() {
        let (mut rx, cmd_tx) = start_traceroute().await;
        until_complete(&mut rx).await;

        // Esc / Enter on the traceroute screen send Resume.
        cmd_tx.send(EngineCommand::Resume).await.unwrap();
        let next = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(
            matches!(&next, Ok(Some(e)) if is_latency(e)),
            "pings did not resume: {next:?}"
        );
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stop_during_traceroute_ends_it_and_closes_the_channel() {
        let (mut rx, cmd_tx) = start_traceroute().await;
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
        let mut after = Vec::new();
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(e) = rx.recv().await {
                after.push(e);
            }
        })
        .await;
        assert!(closed.is_ok(), "channel still open after Stop: {after:?}");
        assert!(hops(&after).is_empty(), "hops after Stop: {after:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn second_start_traceroute_replaces_the_first() {
        let (mut rx, cmd_tx) = start_traceroute().await;
        cmd_tx
            .send(EngineCommand::StartTraceroute(MOCK_TARGET.to_string()))
            .await
            .unwrap();
        let events = until_complete(&mut rx).await;
        assert_eq!(hops(&events).len(), 3, "duplicate hops: {events:?}");

        // No second Complete from the cancelled first run.
        let late = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Some(TelemetryEvent::TracerouteComplete) => return true,
                    Some(_) => {}
                    None => return false,
                }
            }
        })
        .await;
        assert!(!matches!(late, Ok(true)), "the first run also completed");
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn resume_during_traceroute_stops_remaining_hops() {
        let (mut rx, cmd_tx) = start_traceroute().await;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(TelemetryEvent::TracerouteHop(_))) => break,
                Ok(Some(_)) => {}
                other => panic!("no first hop: {other:?}"),
            }
        }
        cmd_tx.send(EngineCommand::Resume).await.unwrap();

        // Hops from the abandoned run must not leak into the ping view.
        let mut after = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(e) = rx.recv().await {
                after.push(e);
            }
        })
        .await;
        assert!(after.iter().any(is_latency), "pings did not resume");
        assert!(
            !after.iter().any(|e| matches!(
                e,
                TelemetryEvent::TracerouteHop(_) | TelemetryEvent::TracerouteComplete
            )),
            "traceroute kept running after Resume: {after:?}"
        );
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
    }

    #[tokio::test]
    async fn start_returns_immediately_and_emits_telemetry() {
        let (tx, mut rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let engine = MockEngine {
            interval: Duration::from_millis(5),
        };

        // Must not block: this was the `--mock` hang.
        tokio::time::timeout(Duration::from_secs(1), engine.start(tx, cmd_rx))
            .await
            .expect("start() should return without waiting for Stop");

        let first = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(matches!(
            first,
            Ok(Some(TelemetryEvent::Ping {
                sequence_number: 1,
                ..
            }))
        ));

        cmd_tx.send(EngineCommand::Stop).await.unwrap();
        // After Stop the task drops its sender, so the channel drains and closes.
        let closed = tokio::time::timeout(Duration::from_secs(1), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        assert!(closed.is_ok(), "telemetry channel should close after Stop");
    }

    #[test]
    fn sample_loses_every_twentieth_icmp_probe() {
        assert!(matches!(sample(20).0, Err(ProbeError::IcmpTimeout)));
        assert!(sample(19).0.is_ok());
        assert!(sample(20).1.is_ok());
    }

    #[test]
    fn sample_returns_sawtooth_values() {
        let values = |seq| {
            let (icmp, tcp) = sample(seq);
            (icmp.ok(), tcp.ok())
        };
        assert_eq!(values(1), (Some(27.0), Some(32.0)));
        // (13 * 7) % 13 == 0: the sawtooth wraps back to its 20 ms floor.
        assert_eq!(values(13), (Some(20.0), Some(25.0)));
    }
}
