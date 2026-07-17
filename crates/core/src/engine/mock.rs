use tokio::sync::mpsc;
use crate::probe::TelemetryEvent;
use super::core_engine::EngineCommand;
use super::NetworkEngine;

#[derive(Default)]
pub struct MockEngine;



impl NetworkEngine for MockEngine {
    async fn start(self, _tx: mpsc::Sender<TelemetryEvent>, mut cmd_rx: mpsc::Receiver<EngineCommand>) {
        tracing::info!("MockEngine started. Simulating engine commands...");
        while let Some(cmd) = cmd_rx.recv().await {
                tracing::info!("MockEngine received command: {:?}", cmd);
                if cmd == EngineCommand::Stop {
                    break;
                }
            }
            tracing::info!("MockEngine stopped.");
    }
}
