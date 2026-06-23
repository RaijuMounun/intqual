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

/// Core network measurement engine executing a concurrent dual-probing strategy.
/// 
/// It dispatches both TCP Handshake and Unprivileged ICMP Datagram probes simultaneously.
/// This approach ensures high observability of both network-layer and application-layer latency
/// without requiring root privileges in modern OS environments.
pub struct CoreEngine {
    /// Shared reference to the target hostname or IP, preventing excessive allocations across micro-tasks.
    pub target_ip: Arc<String>,
    pub target_port: u16,
    pub interval: Duration,
    pub timeout: Duration,
}

impl CoreEngine {
    /// Instantiates a new measurement engine with the provided configuration parameters.
    pub fn new(target_ip: String, target_port: u16, interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            target_ip: Arc::new(target_ip),
            target_port,
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Starts the asynchronous measurement loop.
    /// 
    /// Performs pre-flight DNS resolution to ensure DNS lookup latency does not skew 
    /// the TCP handshake metrics. Subsequently, it spawns a detached worker loop that 
    /// dispatches concurrent probes at the specified interval.
    pub async fn start(self, tx: mpsc::Sender<NetworkMetrics>) {
        let addr_string = format!("{}:{}", self.target_ip, self.target_port);
        
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

        // Generates a stateless, pseudo-random identifier for ICMP packets based on the system clock.
        let icmp_identifier = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::default())
            .subsec_nanos() % 65535) as u16;

        tokio::spawn(async move {
            let mut sequence_counter: u64 = 0;
            let mut interval_timer = tokio::time::interval(self.interval);

            loop {
                interval_timer.tick().await;
                sequence_counter += 1;

                let current_seq = sequence_counter;
                let target_ip_clone = Arc::clone(&self.target_ip);
                let timeout_duration = self.timeout;
                let tx_clone = tx.clone();
                let target_addr = resolved_addr;
                
                let icmp_seq = (current_seq % 65535) as u16;

                // Isolate each interval tick into its own concurrent task to prevent head-of-line blocking
                // in case a specific probe encounters severe packet drops.
                tokio::spawn(async move {
                    let start_time = Instant::now();
                    
                    // --- SUB-TASK A: Asynchronous TCP Handshake Probe ---
                    let tcp_ping_result = match tokio::time::timeout(
                        timeout_duration, 
                        TcpStream::connect(target_addr)
                    ).await {
                        Ok(Ok(stream)) => {
                            let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
                            
                            // Offload socket closure to a blocking thread pool to prevent the OS-level 
                            // TCP RST (Linger) from stalling the asynchronous reactor. This actively 
                            // prevents TIME_WAIT socket exhaustion during high-frequency polling.
                            tokio::task::spawn_blocking(move || {
                                let sock_ref = socket2::SockRef::from(&stream);
                                let _ = sock_ref.set_linger(Some(Duration::from_secs(0)));
                                drop(stream);
                            });

                            Ok(elapsed)
                        },
                        Ok(Err(e)) => Err(format!("Socket Error: {}", e)),
                        Err(_) => Err("TCP Timeout".to_string()),
                    };

                    // --- SUB-TASK B: Synchronous OS-level ICMP Probe ---
                    let icmp_target = target_addr;
                    
                    // Offload blocking syscalls to Tokio's specialized blocking thread pool.
                    let icmp_ping_result = tokio::task::spawn_blocking(move || {
                        let icmp_start = Instant::now();
                        
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

                        // Utilize an uninitialized memory array to eliminate the CPU overhead of 
                        // zeroing out the receive buffer prior to polling.
                        let mut buf = [MaybeUninit::uninit(); 128];
                        
                        // Polling loop: safely drops unrelated alien packets and awaits the matching sequence reply.
                        loop {
                            match socket.recv_from(&mut buf) {
                                Ok((size, _)) => {
                                    let initialized_buf = unsafe {
                                        std::slice::from_raw_parts(buf.as_ptr() as *const u8, size)
                                    };

                                    if let Ok(reply) = IcmpEchoReply::decode(initialized_buf) {
                                        if reply.sequence_number == icmp_seq {
                                            return Ok(icmp_start.elapsed().as_secs_f64() * 1000.0);
                                        }
                                    }

                                    if icmp_start.elapsed() > timeout_duration {
                                        return Err("ICMP Timeout".to_string());
                                    }
                                },
                                Err(_) => return Err("ICMP Timeout".to_string()),
                            }
                        }
                    }).await.unwrap_or_else(|_| Err("Thread Panicked".to_string()));

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

                    let _ = tx_clone.send(metrics).await;
                });
            }
        });
    }
}