use super::NetworkEngine;
use super::core_engine::EngineCommand;
use crate::models::ProbeError;
use crate::probe::TelemetryEvent;
use std::time::Duration;
use tokio::sync::mpsc;

const MOCK_TARGET: &str = "mock.local";

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

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(EngineCommand::Stop) | None => break,
                            Some(EngineCommand::Resume) => paused = false,
                            Some(cmd) => {
                                // Pause, and the bandwidth/traceroute modes, which are not simulated.
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

#[cfg(test)]
mod tests {
    use super::*;

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
