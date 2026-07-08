pub mod widgets;

use crate::engine::core_engine::EngineCommand;
use crate::models::{PingMetrics, BandwidthProgress, ProbeError};
use crate::probe::TelemetryEvent;
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
use widgets::AppWidget;

const HISTORY_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub enum AppMode {
    Ping,
    BandwidthTesting(BandwidthProgress),
    Traceroute,
}

pub struct AppState {
    pub history: Vec<Option<PingMetrics>>,
    pub latest_sequence: u64,
    pub mode: AppMode,
    pub last_speed_test: Option<(f64, f64)>, // (Down, Up)
    pub last_error: Option<String>,
    pub icmp_data: Vec<(f64, f64)>,
    pub tcp_data: Vec<(f64, f64)>,
    pub jitter_history: Vec<u64>,
    pub max_ping: f64,
    pub active_widgets: Vec<Box<dyn AppWidget>>,
    pub traceroute_hops: Vec<crate::models::TracerouteHop>,
    pub traceroute_complete: bool,
    pub current_target_ip: String,
}

impl AppState {
    pub fn new() -> Self {
        let mut history = Vec::with_capacity(HISTORY_SIZE);
        for _ in 0..HISTORY_SIZE {
            history.push(None);
        }
        Self {
            history,
            latest_sequence: 0,
            mode: AppMode::Ping,
            last_speed_test: None,
            last_error: None,
            icmp_data: Vec::with_capacity(HISTORY_SIZE),
            tcp_data: Vec::with_capacity(HISTORY_SIZE),
            jitter_history: Vec::with_capacity(HISTORY_SIZE),
            max_ping: 50.0,
            active_widgets: vec![Box::new(widgets::latency::LatencyDashboardWidget)],
            traceroute_hops: Vec::new(),
            traceroute_complete: false,
            current_target_ip: String::new(),
        }
    }

    pub fn update_icmp(&mut self, seq: u64, target_ip: String, result: std::result::Result<f64, ProbeError>, timestamp: u64) {
        self.current_target_ip = target_ip.clone();
        if seq > self.latest_sequence {
            self.latest_sequence = seq;
        }
        let index = (seq % HISTORY_SIZE as u64) as usize;
        if let Some(ref mut metric) = self.history[index] {
            if metric.sequence_number == seq {
                metric.icmp_ping = result;
                metric.timestamp = timestamp;
            } else {
                self.history[index] = Some(PingMetrics {
                    sequence_number: seq,
                    target_ip,
                    icmp_ping: result,
                    tcp_ping: Err(ProbeError::TcpTimeout),
                    timestamp,
                });
            }
        } else {
            self.history[index] = Some(PingMetrics {
                sequence_number: seq,
                target_ip,
                icmp_ping: result,
                tcp_ping: Err(ProbeError::TcpTimeout),
                timestamp,
            });
        }
    }

    pub fn update_tcp(&mut self, seq: u64, target_ip: String, result: std::result::Result<f64, ProbeError>, timestamp: u64) {
        self.current_target_ip = target_ip.clone();
        if seq > self.latest_sequence {
            self.latest_sequence = seq;
        }
        let index = (seq % HISTORY_SIZE as u64) as usize;
        if let Some(ref mut metric) = self.history[index] {
            if metric.sequence_number == seq {
                metric.tcp_ping = result;
                metric.timestamp = timestamp;
            } else {
                self.history[index] = Some(PingMetrics {
                    sequence_number: seq,
                    target_ip,
                    icmp_ping: Err(ProbeError::IcmpTimeout),
                    tcp_ping: result,
                    timestamp,
                });
            }
        } else {
            self.history[index] = Some(PingMetrics {
                sequence_number: seq,
                target_ip,
                icmp_ping: Err(ProbeError::IcmpTimeout),
                tcp_ping: result,
                timestamp,
            });
        }
    }

    /// Computes diagnostic aggregations (Packet Loss, Avg Jitter) dynamically.
    /// O(N) Compute: Calculating stats on-the-fly during the render loop is intentionally
    /// chosen over maintaining stateful counters. It guarantees mathematical accuracy based
    /// strictly on the visible window and eliminates memory duplication overhead.
    pub fn calculate_stats(&self) -> (f64, f64) {
        let mut loss_count = 0;
        let mut total_count = 0;

        let mut last_ping: Option<f64> = None;
        let mut jitter_sum = 0.0;
        let mut jitter_count = 0;

        let start_seq = self.latest_sequence.saturating_sub(HISTORY_SIZE as u64);

        for seq in start_seq..=self.latest_sequence {
            let idx = (seq % HISTORY_SIZE as u64) as usize;
            if let Some(ref metric) = self.history[idx] {
                if metric.sequence_number == seq {
                    total_count += 1;

                    match metric.icmp_ping {
                        Ok(ping) => {
                            if let Some(last) = last_ping {
                                jitter_sum += (ping - last).abs();
                                jitter_count += 1;
                            }
                            last_ping = Some(ping);
                        }
                        Err(ProbeError::IcmpTimeout) => {
                            loss_count += 1;
                        }
                        Err(_) => {
                            // Systemic failures (PermissionDenied, Socket) are not network loss
                        }
                    }
                }
            }
        }

        let loss_pct = if total_count > 0 {
            (loss_count as f64 / total_count as f64) * 100.0
        } else {
            0.0
        };

        let avg_jitter = if jitter_count > 0 {
            jitter_sum / jitter_count as f64
        } else {
            0.0
        };

        (loss_pct, avg_jitter)
    }

    pub fn prepare_render_data(&mut self) {
        self.icmp_data.clear();
        self.tcp_data.clear();
        self.jitter_history.clear();
        self.max_ping = 50.0;

        let start_seq = self.latest_sequence.saturating_sub(HISTORY_SIZE as u64);
        let mut last_icmp_ping: Option<f64> = None;

        for seq in start_seq..=self.latest_sequence {
            let idx = (seq % HISTORY_SIZE as u64) as usize;
            if let Some(ref metric) = self.history[idx] {
                if metric.sequence_number == seq {
                    if let Ok(ping) = metric.icmp_ping {
                        self.icmp_data.push((seq as f64, ping));
                        if ping > self.max_ping {
                            self.max_ping = ping;
                        }

                        if let Some(last) = last_icmp_ping {
                            let j = (ping - last).abs();
                            self.jitter_history.push(j.round() as u64);
                        } else {
                            self.jitter_history.push(0);
                        }
                        last_icmp_ping = Some(ping);
                    } else {
                        self.icmp_data.push((seq as f64, 0.0));
                        self.jitter_history.push(0);
                    }

                    if let Ok(ping) = metric.tcp_ping {
                        self.tcp_data.push((seq as f64, ping));
                        if ping > self.max_ping {
                            self.max_ping = ping;
                        }
                    }
                }
            } else {
                self.jitter_history.push(0);
            }
        }
    }
}

/// The main synchronous event loop for the Terminal User Interface.
pub fn run_app(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    cmd_tx: mpsc::Sender<EngineCommand>,
    _tx: mpsc::Sender<TelemetryEvent>,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = AppState::new();

    // Initial render to prevent a blank screen before the first packet arrives.
    terminal.draw(|frame| draw_ui(frame, &app))?;

    // THE IMMEDIATE-MODE GUI LOOP
    loop {
        // Dirty Flag: Terminates redundant GPU/Compositor rendering.
        // Re-rendering 60+ FPS in a terminal causes severe compositor timeouts
        // (e.g., KDE KWin, WebGL environments) and CPU spikes. We strictly only issue
        // a draw command if new data has actively altered the state.
        let mut state_changed = false;

        while let Ok(event) = rx.try_recv() {
            match event {
                TelemetryEvent::Ping { sequence_number, target_ip, result, timestamp } => {
                    app.update_icmp(sequence_number, target_ip, result, timestamp);
                    state_changed = true;
                }
                TelemetryEvent::Tcp { sequence_number, target_ip, result, timestamp } => {
                    app.update_tcp(sequence_number, target_ip, result, timestamp);
                    state_changed = true;
                }
                TelemetryEvent::Bandwidth(progress) => {
                    app.last_error = None;
                    if let BandwidthProgress::Finished { download_mbps, upload_mbps } = &progress {
                        app.last_speed_test = Some((*download_mbps, *upload_mbps));
                    }
                    app.mode = AppMode::BandwidthTesting(progress);
                    app.active_widgets = vec![Box::new(crate::ui::widgets::bandwidth::BandwidthWidget)];
                    state_changed = true;
                }
                TelemetryEvent::BandwidthError(err) => {
                    if let AppMode::Ping = app.mode {
                        // Ignore stale errors if already manually aborted
                    } else {
                        app.mode = AppMode::Ping;
                        app.active_widgets = vec![Box::new(crate::ui::widgets::latency::LatencyDashboardWidget)];
                        app.last_error = Some(err.to_string());
                        if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Resume) {
                            tracing::error!("Failed to send EngineCommand: {}", e);
                        }
                        state_changed = true;
                    }
                }
                TelemetryEvent::Fatal(err) => {
                    app.mode = AppMode::Ping;
                    app.active_widgets = vec![Box::new(crate::ui::widgets::latency::LatencyDashboardWidget)];
                    app.last_error = Some(format!("FATAL ERROR: {}", err));
                    let _ = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Stop);
                    state_changed = true;
                }
                TelemetryEvent::TracerouteHop(hop) => {
                    app.traceroute_hops.push(hop);
                    state_changed = true;
                }
                TelemetryEvent::TracerouteComplete => {
                    app.traceroute_complete = true;
                    state_changed = true;
                }
                TelemetryEvent::TracerouteError(err) => {
                    app.last_error = Some(format!("Traceroute Error: {}", err));
                    state_changed = true;
                }
            }
        }

        if state_changed {
            app.prepare_render_data();
            terminal.draw(|frame| draw_ui(frame, &app))?;
        }

        // Throttle the event poller to 50ms (20 FPS) to drastically reduce idle CPU footprint.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q') {
                        let _ = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Stop);
                        break;
                    } else if key.code == KeyCode::Char('s') {
                        if !matches!(app.mode, AppMode::BandwidthTesting(_)) {
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::StartBandwidthTest) {
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Char('t') {
                        if !app.current_target_ip.is_empty() {
                            app.mode = AppMode::Traceroute;
                            app.traceroute_hops.clear();
                            app.traceroute_complete = false;
                            app.last_error = None;
                            app.active_widgets = vec![Box::new(crate::ui::widgets::traceroute::TracerouteWidget::default())];
                            if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::StartTraceroute(app.current_target_ip.clone())) {
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Esc {
                        if matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Downloading {..} | BandwidthProgress::Uploading {..}) | AppMode::Traceroute) {
                            app.mode = AppMode::Ping;
                            app.active_widgets = vec![Box::new(crate::ui::widgets::latency::LatencyDashboardWidget)];
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Resume) {
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Enter {
                        if matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..})) {
                            app.mode = AppMode::Ping;
                            app.active_widgets = vec![Box::new(crate::ui::widgets::latency::LatencyDashboardWidget)];
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Resume) {
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
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
    let is_finished = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..}));
    let is_traceroute = matches!(app.mode, AppMode::Traceroute);

    let nav_text = if is_testing {
        "[Q: Quit] | [Esc: Cancel Test]"
    } else if is_finished {
        "[Q: Quit] | [Enter: Return to Ping]"
    } else if is_traceroute {
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

    for widget in &app.active_widgets {
        widget.render(frame, screen_layout[1], app);
    }
}
