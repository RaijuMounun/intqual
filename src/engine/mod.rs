pub mod core_engine;

use crate::probe::TelemetryEvent;
use std::sync::mpsc;

/// The core contract for network engine implementations.
/// 
/// Implementing this trait allows different probing strategies (e.g., strictly unprivileged, 
/// strictly raw, or hybrid fallback) to be injected interchangeably into the application lifecycle.
pub trait NetworkEngine {
    /// Ignites the network monitoring loop.
    /// 
    /// The engine spawns background tasks and continuously streams measurement results 
    /// back to the provided MPSC transmission channel.
    fn start(self, tx: mpsc::Sender<TelemetryEvent>) -> impl std::future::Future<Output = ()> + Send;
}

pub use core_engine::CoreEngine;