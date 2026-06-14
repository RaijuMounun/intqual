pub mod fallback;

use crate::models::NetworkMetrics;
use std::sync::mpsc;

/// The core contract for all network engine implementations.
/// Any engine (Raw, Fallback, etc.) must implement this trait to be injected into the system.
pub trait NetworkEngine {
    /// Starts the network monitoring loop.
    /// 
    /// # Arguments
    /// 
    /// * `tx` - The transmission half of an MPSC channel used to push metrics to the consumer (UI).
    fn start(&self, tx: mpsc::Sender<NetworkMetrics>);
}