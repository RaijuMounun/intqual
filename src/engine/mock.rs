use tokio::sync::mpsc;
use crate::probe::TelemetryEvent;
use super::core_engine::EngineCommand;
use super::NetworkEngine;

#[derive(Default)]
pub struct MockEngine;

impl MockEngine {
    pub fn new() -> Self {
        Self
    }
}

impl NetworkEngine for MockEngine {
    fn start(self: Box<Self>, _tx: mpsc::Sender<TelemetryEvent>, mut cmd_rx: mpsc::Receiver<EngineCommand>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            tracing::info!("MockEngine started. Simulating engine commands...");
            while let Some(cmd) = cmd_rx.recv().await {
                tracing::info!("MockEngine received command: {:?}", cmd);
                if cmd == EngineCommand::Stop {
                    break;
                }
            }
            tracing::info!("MockEngine stopped.");
        })
    }
}
