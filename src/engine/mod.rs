pub mod core_engine;
pub mod mock;

use crate::probe::TelemetryEvent;
use crate::engine::core_engine::EngineCommand;
use tokio::sync::mpsc;

use std::future::Future;
use std::pin::Pin;

pub trait NetworkEngine: Send {
    fn start(self: Box<Self>, tx: mpsc::Sender<TelemetryEvent>, cmd_rx: mpsc::Receiver<EngineCommand>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

pub use core_engine::CoreEngine;
pub use mock::MockEngine;