pub mod widgets;

use intqual_core::engine::core_engine::EngineCommand;
use intqual_core::models::{PingMetrics, BandwidthProgress, ProbeError, TelemetryEvent};
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    layout::Alignment,
    prelude::*,
    widgets::Paragraph,
};
use std::io::{Result, stdout};
use std::time::Duration;
use tokio::sync::mpsc;
use std::collections::VecDeque;
use widgets::ActiveWidget;

const HISTORY_SIZE: usize = 100;

#[must_use]
pub enum RenderAction {
    Redraw,
    Skip,
}

#[derive(Debug, Clone)]
pub enum DnsStatus {
    Resolving,
    Resolved(String),
    Failed,
}

#[derive(Debug, Clone)]
pub enum AppMode {
    Ping,
    BandwidthTesting(BandwidthProgress),
    Traceroute,
    Error(String),
}

#[derive(Default, Debug, Clone)]
pub struct NetworkStats {
    pub loss_pct: f64,
    pub avg_jitter: f64,
    pub min_ping: f64,
    pub max_ping: f64,
    pub current_jitter: f64,
    
    pub total_count: u64,
    pub loss_count: u64,
    pub jitter_sum: f64,
    pub jitter_count: u64,
    pub last_ping: Option<f64>,
}

impl NetworkStats {
    pub fn update(&mut self, result: &std::result::Result<f64, ProbeError>) {
        self.total_count += 1;
        match result {
            Ok(ping) => {
                let p = *ping;
                if p > self.max_ping { self.max_ping = p; }
                if self.min_ping == 0.0 || p < self.min_ping { self.min_ping = p; }
                
                if let Some(last) = self.last_ping {
                    self.current_jitter = (p - last).abs();
                    self.jitter_sum += self.current_jitter;
                    self.jitter_count += 1;
                    self.avg_jitter = self.jitter_sum / self.jitter_count as f64;
                } else {
                    self.current_jitter = 0.0;
                }
                self.last_ping = Some(p);
            }
            Err(ProbeError::IcmpTimeout) | Err(ProbeError::TcpTimeout) => {
                self.loss_count += 1;
            }
            Err(e) => {
                tracing::warn!("UI Parsing fallback triggered: {:?}", e);
            }
        }
        
        if self.total_count > 0 {
            self.loss_pct = (self.loss_count as f64 / self.total_count as f64) * 100.0;
        }
    }
}

pub struct AppState {
    pub latest_sequence: u64,
    pub mode: AppMode,
    pub last_speed_test: Option<(f64, f64)>, // (Down, Up)
    pub last_error: Option<String>,
    
    // Push-based stats and chart data
    pub icmp_stats: NetworkStats,
    pub tcp_stats: NetworkStats,
    pub icmp_data: VecDeque<(f64, f64)>,
    pub tcp_data: VecDeque<(f64, f64)>,
    pub jitter_history: VecDeque<u64>,
    pub chart_max_ping: f64,
    
    // For single-frame display
    pub latest_metric: Option<PingMetrics>,
    
    pub active_widget: ActiveWidget,
    pub traceroute_hops: Vec<intqual_core::models::TracerouteHop>,
    pub traceroute_complete: bool,
    pub dns_status: std::collections::HashMap<String, DnsStatus>,
    pub current_target_ip: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            latest_sequence: 0,
            mode: AppMode::Ping,
            last_speed_test: None,
            last_error: None,
            icmp_stats: NetworkStats::default(),
            tcp_stats: NetworkStats::default(),
            icmp_data: VecDeque::with_capacity(HISTORY_SIZE * 2),
            tcp_data: VecDeque::with_capacity(HISTORY_SIZE * 2),
            jitter_history: VecDeque::with_capacity(HISTORY_SIZE * 2),
            chart_max_ping: 50.0,
            latest_metric: None,
            active_widget: ActiveWidget::Latency,
            traceroute_hops: Vec::new(),
            traceroute_complete: false,
            dns_status: std::collections::HashMap::new(),
            current_target_ip: String::new(),
        }
    }

    pub fn reset_ping_state(&mut self) {
        self.icmp_stats = NetworkStats::default();
        self.tcp_stats = NetworkStats::default();
        self.icmp_data.clear();
        self.tcp_data.clear();
        self.jitter_history.clear();
        self.chart_max_ping = 50.0;
        self.latest_sequence = 0;
        self.latest_metric = None;
    }

    fn push_chart_data<T>(vec: &mut VecDeque<T>, item: T) {
        vec.push_back(item);
        if vec.len() > HISTORY_SIZE {
            vec.pop_front();
        }
        vec.make_contiguous();
    }

    fn update_chart_max_ping(&mut self) {
        let mut max = 50.0;
        for &(_, p) in &self.icmp_data {
            if p > max { max = p; }
        }
        for &(_, p) in &self.tcp_data {
            if p > max { max = p; }
        }
        self.chart_max_ping = max;
    }

    pub fn handle_event(&mut self, event: TelemetryEvent, _cmd_tx: &mpsc::Sender<EngineCommand>) -> RenderAction {
        match event {
            TelemetryEvent::Ping { sequence_number, target_ip, result, timestamp } => {
                if self.current_target_ip != target_ip {
                    self.current_target_ip = target_ip.clone();
                }
                if sequence_number > self.latest_sequence {
                    self.latest_sequence = sequence_number;
                }
                
                self.icmp_stats.update(&result);
                
                let p_val = match &result {
                    Ok(p) => *p,
                    Err(e) => {
                        tracing::warn!("UI Parsing fallback triggered: {:?}", e);
                        0.0
                    },
                };
                Self::push_chart_data(&mut self.icmp_data, (sequence_number as f64, p_val));
                
                if result.is_ok() {
                    let j = self.icmp_stats.current_jitter.round() as u64;
                    Self::push_chart_data(&mut self.jitter_history, j);
                    
                    if let Ok(ping) = result
                        && ping > self.chart_max_ping {
                            self.chart_max_ping = ping;
                    }
                } else {
                    Self::push_chart_data(&mut self.jitter_history, 0);
                }
                
                // If this packet fell out of the window and it was the max, recompute
                if self.icmp_data.len() == HISTORY_SIZE {
                    self.update_chart_max_ping();
                }

                if let Some(ref mut metric) = self.latest_metric {
                    metric.sequence_number = sequence_number;
                    metric.target_ip = target_ip;
                    metric.icmp_ping = result;
                    metric.timestamp = timestamp;
                } else {
                    self.latest_metric = Some(PingMetrics {
                        sequence_number,
                        target_ip,
                        icmp_ping: result,
                        tcp_ping: Err(ProbeError::TcpTimeout),
                        timestamp,
                    });
                }
                
                RenderAction::Redraw
            }
            TelemetryEvent::Tcp { sequence_number, target_ip, result, timestamp } => {
                if self.current_target_ip != target_ip {
                    self.current_target_ip = target_ip.clone();
                }
                if sequence_number > self.latest_sequence {
                    self.latest_sequence = sequence_number;
                }
                
                self.tcp_stats.update(&result);
                
                let p_val = match &result {
                    Ok(p) => *p,
                    Err(e) => {
                        tracing::warn!("UI Parsing fallback triggered: {:?}", e);
                        0.0
                    },
                };
                Self::push_chart_data(&mut self.tcp_data, (sequence_number as f64, p_val));
                
                if let Ok(ping) = result
                    && ping > self.chart_max_ping {
                        self.chart_max_ping = ping;
                }
                
                if self.tcp_data.len() == HISTORY_SIZE {
                    self.update_chart_max_ping();
                }

                if let Some(ref mut metric) = self.latest_metric {
                    metric.sequence_number = sequence_number;
                    metric.target_ip = target_ip;
                    metric.tcp_ping = result;
                    metric.timestamp = timestamp;
                } else {
                    self.latest_metric = Some(PingMetrics {
                        sequence_number,
                        target_ip,
                        icmp_ping: Err(ProbeError::IcmpTimeout),
                        tcp_ping: result,
                        timestamp,
                    });
                }
                
                RenderAction::Redraw
            }
            TelemetryEvent::Bandwidth(progress) => {
                self.last_error = None;
                if let BandwidthProgress::Finished { download_mbps, upload_mbps } = progress {
                    self.last_speed_test = Some((download_mbps, upload_mbps));
                }
                self.mode = AppMode::BandwidthTesting(progress);
                self.active_widget = ActiveWidget::Bandwidth;
                RenderAction::Redraw
            }
            TelemetryEvent::BandwidthError(err) => {
                if matches!(self.mode, AppMode::BandwidthTesting(_)) {
                    self.mode = AppMode::BandwidthTesting(BandwidthProgress::Failed(err.to_string()));
                    RenderAction::Redraw
                } else {
                    RenderAction::Skip
                }
            }
            TelemetryEvent::Fatal(err) => {
                self.mode = AppMode::Error(err.to_string());
                self.active_widget = ActiveWidget::Error;
                self.last_error = Some(format!("FATAL ERROR: {}", err));
                RenderAction::Redraw
            }
            TelemetryEvent::TracerouteHop(hop) => {
                if let Some(ref ip) = hop.ip_address
                    && !self.dns_status.contains_key(ip) {
                        self.dns_status.insert(ip.clone(), DnsStatus::Resolving);
                }
                self.traceroute_hops.push(hop);
                RenderAction::Redraw
            }
            TelemetryEvent::DnsResolved { ip, hostname } => {
                let status = match hostname {
                    Some(name) => DnsStatus::Resolved(name),
                    None => DnsStatus::Failed,
                };
                self.dns_status.insert(ip, status);
                RenderAction::Redraw
            }
            TelemetryEvent::TracerouteComplete => {
                self.traceroute_complete = true;
                RenderAction::Redraw
            }
            TelemetryEvent::TracerouteError(err) => {
                self.last_error = Some(format!("Traceroute Error: {}", err));
                RenderAction::Redraw
            }
        }
    }
}

/// The main synchronous event loop for the Terminal User Interface.
pub fn run_app(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    cmd_tx: mpsc::Sender<EngineCommand>,
    tx: mpsc::Sender<TelemetryEvent>,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = AppState::new();

    // Initial render to prevent a blank screen before the first packet arrives.
    terminal.draw(|frame| draw_ui(frame, &app))?;

    // THE IMMEDIATE-MODE GUI LOOP
    loop {
        let mut should_redraw = false;

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if matches!(app.handle_event(event, &cmd_tx), RenderAction::Redraw) {
                        should_redraw = true;
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if disconnected && !matches!(app.mode, AppMode::Error(_)) {
            break;
        }

        if should_redraw {
            terminal.draw(|frame| draw_ui(frame, &app))?;
        }

        // Throttle the event poller to 50ms (20 FPS) to drastically reduce idle CPU footprint.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q') {
                        if let Err(e) = cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::Stop)
                            && matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                break;
                            }
                        break;
                    } else if key.code == KeyCode::Char('s') {
                        if !matches!(app.mode, AppMode::BandwidthTesting(_)) {
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::StartBandwidthTest) {
                                if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                    break;
                                }
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Char('t') {
                        if !app.current_target_ip.is_empty() {
                            app.mode = AppMode::Traceroute;
                            app.traceroute_hops.clear();
                            app.dns_status.clear();
                            app.traceroute_complete = false;
                            app.last_error = None;
                            app.active_widget = ActiveWidget::Traceroute;
                            
                            let _ = cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::Pause);
                            
                            let _ = disable_raw_mode();
                            let _ = stdout().execute(LeaveAlternateScreen);
                            
                            let exe_path = std::env::current_exe().unwrap_or_else(|_| "intqual".into());
                            let output = std::process::Command::new("sudo")
                                .arg("-E")
                                .arg(exe_path)
                                .arg("--worker-mode")
                                .arg("traceroute")
                                .arg(app.current_target_ip.clone())
                                .output();
                                
                            let _ = stdout().execute(EnterAlternateScreen);
                            let _ = enable_raw_mode();
                            let _ = terminal.clear();
                            
                            match output {
                                Ok(out) if out.status.success() => {
                                    if let Ok(hops) = serde_json::from_slice::<Vec<intqual_core::models::TracerouteHop>>(&out.stdout) {
                                        for hop in hops {
                                            app.traceroute_hops.push(hop.clone());
                                            
                                            if let Some(ip) = hop.ip_address {
                                                if !app.dns_status.contains_key(&ip) {
                                                    app.dns_status.insert(ip.clone(), DnsStatus::Resolving);
                                                }
                                                let tx_dns = tx.clone();
                                                let ip_clone = ip.clone();
                                                tokio::runtime::Handle::current().spawn(async move {
                                                    let hostname = tokio::task::spawn_blocking(move || {
                                                        match ip_clone.parse::<std::net::IpAddr>() {
                                                            Ok(addr) => dns_lookup::lookup_addr(&addr).ok(),
                                                            Err(_) => None,
                                                        }
                                                    }).await.unwrap_or(None);
                                                    
                                                    let _ = tx_dns.send(TelemetryEvent::DnsResolved {
                                                        ip,
                                                        hostname,
                                                    }).await;
                                                });
                                            }
                                        }
                                        app.traceroute_complete = true;
                                    } else {
                                        app.last_error = Some("Failed to parse worker output".to_string());
                                    }
                                }
                                Ok(out) => {
                                    app.last_error = Some(format!("Worker failed: {}", String::from_utf8_lossy(&out.stderr)));
                                }
                                Err(e) => {
                                    app.last_error = Some(format!("Failed to start worker: {}", e));
                                }
                            }
                        }
                    } else if key.code == KeyCode::Esc {
                        if matches!(app.mode, AppMode::BandwidthTesting(_) | AppMode::Traceroute | AppMode::Error(_)) {
                            app.reset_ping_state();
                            app.mode = AppMode::Ping;
                            app.active_widget = ActiveWidget::Latency;
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::Resume) {
                                if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                    break;
                                }
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Enter
                        && matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..}) | AppMode::BandwidthTesting(BandwidthProgress::Failed(_)) | AppMode::Traceroute | AppMode::Error(_)) {
                            app.reset_ping_state();
                            app.mode = AppMode::Ping;
                            app.active_widget = ActiveWidget::Latency;
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::Resume) {
                                if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                    break;
                                }
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                    }
                }
                Event::Resize(_, _) => {
                    // Force a re-render when the user resizes the terminal window to prevent visual artifacting.
                    terminal.draw(|frame| draw_ui(frame, &app))?;
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Renders the UI frame using a Bento Grid architecture.
fn draw_ui(frame: &mut Frame, app: &AppState) {
    let area = frame.area();

    let screen_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let is_testing = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Downloading {..} | BandwidthProgress::Uploading {..}));
    let is_finished_or_failed = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..} | BandwidthProgress::Failed(_)));
    let is_traceroute = matches!(app.mode, AppMode::Traceroute);

    let is_error = matches!(app.mode, AppMode::Error(_));

    let nav_text = if is_testing {
        "[Q: Quit] | [Esc: Cancel Test]"
    } else if is_finished_or_failed || is_traceroute {
        "[Q: Quit] | [Enter: Return to Ping]"
    } else if is_error {
        "[Q: Quit] | [Esc: Return to Ping]"
    } else {
        "[Q: Quit] | [S: Speed Test] | [T: Traceroute]"
    };

    let top_bar = Paragraph::new(nav_text)
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Right);

    frame.render_widget(top_bar, screen_layout[0]);

    app.active_widget.render(frame, screen_layout[1], app);
}
