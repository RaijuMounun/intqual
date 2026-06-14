/// Represents the aggregated network metrics for a specific target.
/// This struct is completely decoupled from the engine logic and UI.
#[derive(Debug)]
pub struct NetworkMetrics {
    /// The IP address or hostname being monitored (e.g., "8.8.8.8").
    pub target_ip: String,
    
    /// The ICMP ping latency in milliseconds. 
    /// Returns `Ok(f64)` if successful, or an `Err(String)` explaining why it failed.
    pub icmp_ping: Result<f64, String>,
    
    /// The TCP ping latency (handshake duration) in milliseconds.
    /// Returns `Ok(f64)` if successful, or an `Err(String)` explaining why it failed.
    pub tcp_ping: Result<f64, String>,
    
    /// UNIX timestamp of when this metric was recorded. 
    /// Useful for time-series graphs in the UI.
    pub timestamp: u64,
} 