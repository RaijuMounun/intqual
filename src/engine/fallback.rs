use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use crate::models::NetworkMetrics;

/// FallbackEngine implements network measurements using standard OS capabilities.
/// It operates without requiring elevated privileges (root/sudo).
pub struct FallbackEngine {
    // Arc (Atomic Reference Counted) ensures thread-safe, zero-cost string sharing 
    // across multiple spawned micro-tasks without cloning the underlying string data.
    pub target_ip: Arc<String>,
    pub target_port: u16,
    pub interval: Duration,
    pub timeout: Duration,
}

impl FallbackEngine {
    /// Constructs a new instance of FallbackEngine.
    /// 
    /// # Arguments
    /// * `target_ip` - The hostname or IP address to monitor.
    /// * `target_port` - The TCP port used for the handshake latency measurement.
    /// * `interval_ms` - The polling interval in milliseconds.
    pub fn new(target_ip: String, target_port: u16, interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            target_ip: Arc::new(target_ip), // Wrap in Arc for efficient concurrent sharing
            target_port,
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Starts the asynchronous execution loop for network monitoring.
    /// Pushes the aggregated metrics into the provided MPSC channel.
    pub async fn start(self, tx: mpsc::Sender<NetworkMetrics>) {
        // The main tick loop runs in its own background task
        tokio::spawn(async move {
            let mut sequence_counter: u64 = 0;
            let mut interval_timer = tokio::time::interval(self.interval);

            loop {
                // Wait for the next tick (e.g., exactly every 200ms)
                interval_timer.tick().await;
                sequence_counter += 1;

                // Clone lightweight references/values for the spawned micro-task
                let current_seq = sequence_counter;
                let target_ip_clone = Arc::clone(&self.target_ip);
                let target_port = self.target_port;
                let timeout_duration = self.timeout;
                let tx_clone = tx.clone();

                // SPAWN: Fire and forget. This prevents the loop from blocking.
                // Even if this specific TCP connection hangs for 1000ms, 
                // the outer loop will continue firing new tasks every 200ms.
                tokio::spawn(async move {
                    let start_time = Instant::now();
                    
                    let tcp_ping_result = match tokio::time::timeout(
                        timeout_duration, 
                        TcpStream::connect((target_ip_clone.as_str(), target_port))
                    ).await {
                        Ok(Ok(_stream)) => Ok(start_time.elapsed().as_secs_f64() * 1000.0),
                        Ok(Err(e)) => Err(format!("Socket Error: {}", e)),
                        Err(_) => Err("TCP Timeout".to_string()),
                    };

                    let icmp_ping_result = Err("Unprivileged ICMP not yet implemented".to_string());

                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();

                    let metrics = NetworkMetrics {
                        sequence_number: current_seq,
                        target_ip: target_ip_clone.to_string(),
                        icmp_ping: icmp_ping_result,
                        tcp_ping: tcp_ping_result,
                        timestamp,
                    };

                    // Send the result. If the receiver is dead, silently ignore 
                    // (the main loop will eventually detect the dead channel and break).
                    let _ = tx_clone.send(metrics).await;
                });
            }
        });
    }
}