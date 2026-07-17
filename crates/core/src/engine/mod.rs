pub mod core_engine;
pub mod mock;

use crate::probe::TelemetryEvent;
use crate::engine::core_engine::EngineCommand;
use tokio::sync::mpsc;

pub trait NetworkEngine: Send {
    fn start(self, tx: mpsc::Sender<TelemetryEvent>, cmd_rx: mpsc::Receiver<EngineCommand>) -> impl std::future::Future<Output = ()> + Send;
}

pub use core_engine::CoreEngine;
pub use mock::MockEngine;