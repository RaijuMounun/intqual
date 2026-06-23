use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use socket2::{Domain, Protocol, Socket, Type};
use crate::models::NetworkMetrics;
use crate::network::icmp::{IcmpEchoRequest, IcmpEchoReply};

/// The `CoreEngine` is responsible for measuring network latency without requiring elevated privileges.
/// It utilizes a concurrent dual-probing strategy (TCP Handshake and Unprivileged ICMP Datagrams)
/// to ensure high observability even in restricted OS environments.
pub struct CoreEngine {
    /// Shared reference to the target IP to avoid unnecessary cloning across spawned micro-tasks.
    pub target_ip: Arc<String>,
    pub target_port: u16,
    pub interval: Duration,
    pub timeout: Duration,
}

impl CoreEngine {
    /// Instantiates a new `CoreEngine` with injected configuration parameters.
    pub fn new(target_ip: String, target_port: u16, interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            target_ip: Arc::new(target_ip),
            target_port,
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Ignites the asynchronous measurement engine.
    /// This method resolves DNS upfront (Pre-flight) to ensure metric purity,
    /// preventing DNS resolution latency from polluting TCP handshake measurements.
    pub async fn start(self, tx: mpsc::Sender<NetworkMetrics>) {
        let addr_string = format!("{}:{}", self.target_ip, self.target_port);
        
        // 1. Pre-flight DNS Resolution
        let resolved_addr: SocketAddr = match tokio::net::lookup_host(&addr_string).await {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    addr
                } else {
                    eprintln!("Fatal Error: DNS returned no addresses for {}", self.target_ip);
                    return;
                }
            },
            Err(e) => {
                eprintln!("Fatal Error: DNS Resolution Failed: {}", e);
                return;
            }
        };

        // Generate a stateless, pseudo-random identifier for ICMP packets based on the system clock.
        let icmp_identifier = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::default())
            .subsec_nanos() % 65535) as u16;

        // 2. Main Metronome Loop (Background Worker)
        tokio::spawn(async move {
            let mut sequence_counter: u64 = 0;
            let mut interval_timer = tokio::time::interval(self.interval);

            loop {
                // Wait for the exact interval tick
                interval_timer.tick().await;
                sequence_counter += 1;

                // Prepare state for the isolated micro-task
                let current_seq = sequence_counter;
                let target_ip_clone = Arc::clone(&self.target_ip);
                let timeout_duration = self.timeout;
                let tx_clone = tx.clone();
                let target_addr = resolved_addr;
                
                let icmp_seq = (current_seq % 65535) as u16;

                // SPAWN: Isolate each tick into its own concurrent task to prevent Head-of-Line blocking.
                tokio::spawn(async move {
                    let start_time = Instant::now();
                    
                    // --- SUB-TASK A: Asynchronous TCP Handshake Probe ---
                    let tcp_ping_result = match tokio::time::timeout(
                        timeout_duration, 
                        TcpStream::connect(target_addr)
                    ).await {
                        Ok(Ok(stream)) => {
                            let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
                            
                            // OPTIMIZATION: Zero-Cost TIME_WAIT prevention.
                            // We offload the socket closure to a blocking thread pool.
                            // This prevents the OS-level TCP RST (Linger) from stalling our async reactor.
                            tokio::task::spawn_blocking(move || {
                                // socket2::SockRef is the modern, safe way to manipulate socket options 
                                // without triggering Tokio's deprecation warnings.
                                let sock_ref = socket2::SockRef::from(&stream);
                                let _ = sock_ref.set_linger(Some(Duration::from_secs(0)));
                                
                                // Dropping the stream here explicitly closes the socket 
                                // and sends the RST packet in the background.
                                drop(stream);
                            });

                            Ok(elapsed)
                        },
                        Ok(Err(e)) => Err(format!("Socket Error: {}", e)),
                        Err(_) => Err("TCP Timeout".to_string()),
                    };

                    // --- SUB-TASK B: Synchronous OS-level ICMP Probe ---
                    let icmp_target = target_addr;
                    // Offload blocking syscalls to Tokio's specialized blocking thread pool
                    // to prevent stalling the main async reactor.
                    let icmp_ping_result = tokio::task::spawn_blocking(move || {
                        let icmp_start = Instant::now();
                        
                        // Attempt to open an unprivileged datagram socket (macOS/Linux compatible)
                        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
                            Ok(s) => s,
                            Err(_) => return Err("Permission Denied / Unsupported".to_string()),
                        };

                        let _ = socket.set_read_timeout(Some(timeout_duration));
                        let _ = socket.set_write_timeout(Some(timeout_duration));

                        let packet = IcmpEchoRequest::new(icmp_identifier, icmp_seq, vec![]);
                        let packet_bytes = packet.encode();

                        if socket.send_to(&packet_bytes, &icmp_target.into()).is_err() {
                            return Err("Send Failed".to_string());
                        }

                        // Utilize uninitialized memory for zero-cost receive buffering
                        let mut buf = [MaybeUninit::uninit(); 128];
                        // SMART POLLING LOOP: Ignore alien packets, wait for OUR reply.
                        loop {
                            match socket.recv_from(&mut buf) {
                                Ok((size, _)) => {
                                    // Safely reconstruct the byte slice from the raw pointer and size
                                    let initialized_buf = unsafe {
                                        std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                                    };

                                    // Validate the packet
                                    if let Ok(reply) = IcmpEchoReply::decode(initialized_buf) {
                                        if reply.sequence_number == icmp_seq {
                                            return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                                        }
                                    }

                                    // If we received an alien packet, check if we still have time left to wait
                                    if icmp_start.elapsed() > timeout_duration {
                                        return Err("ICMP Timeout".to_string());
                                    }
                                },
                                Err(_) => return Err("ICMP Timeout".to_string()), // OS Timeout triggered
                            }
                        }
                    }).await.unwrap_or_else(|_| Err("Thread Panicked".to_string()));

                    // --- PIPELINE AGGREGATION ---
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

                    // Dispatch to the UI consumer. Silently drop if receiver is closed.
                    let _ = tx_clone.send(metrics).await;
                });
            }
        });
    }
}