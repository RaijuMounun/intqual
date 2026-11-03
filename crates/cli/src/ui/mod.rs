pub mod widgets;

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use intqual_core::engine::core_engine::EngineCommand;
use intqual_core::models::{BandwidthProgress, PingMetrics, ProbeError, TelemetryEvent};
use ratatui::{layout::Alignment, prelude::*, widgets::Paragraph};
use std::collections::VecDeque;
use std::io::{Result, stdout};
use std::time::Duration;
use tokio::sync::mpsc;
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
    /// Failed probes that are not loss (see [`is_loss`]): kept out of loss %, but counted
    /// so the UI can show them instead of letting them vanish.
    pub error_count: u64,
    pub jitter_sum: f64,
    pub jitter_count: u64,
    pub last_ping: Option<f64>,
}

/// Whether a failed probe means the packet was lost: a request went out and no valid
/// reply came back.
///
/// Timeouts and ICMP error replies (e.g. destination unreachable from a router) are loss.
/// A refused TCP connection (`Socket`) is not: the RST is the target answering. Neither are
/// `PermissionDenied` and the remaining variants, which are local setup or internal
/// failures. Those go to `error_count` and stay out of the loss % denominator.
fn is_loss(err: &ProbeError) -> bool {
    matches!(
        err,
        ProbeError::IcmpTimeout | ProbeError::TcpTimeout | ProbeError::IcmpError { .. }
    )
}

impl NetworkStats {
    /// Folds one probe result into the running stats.
    ///
    /// Only successes and losses (see [`is_loss`]) are counted in `total_count`; other
    /// errors are logged and counted in `error_count` so they neither inflate nor dilute
    /// loss %.
    /// Jitter follows RFC 3393: it is the difference between two *consecutive* probes that
    /// both succeeded. Any failure breaks the chain, so no pair spans a gap.
    pub fn update(&mut self, result: &std::result::Result<f64, ProbeError>) {
        match result {
            Ok(ping) => {
                self.total_count += 1;
                let p = *ping;
                if p > self.max_ping {
                    self.max_ping = p;
                }
                if self.min_ping == 0.0 || p < self.min_ping {
                    self.min_ping = p;
                }

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
            Err(e) if is_loss(e) => {
                self.total_count += 1;
                self.loss_count += 1;
                self.last_ping = None;
            }
            Err(e) => {
                tracing::warn!("Probe error not counted as loss: {:?}", e);
                self.error_count += 1;
                self.last_ping = None;
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

    /// Switches to the speed-test screen once `StartBandwidthTest` was sent.
    /// Only `BandwidthTesting` mode accepts `Bandwidth` events, so the mode has to be
    /// entered here rather than by the first progress frame.
    pub fn begin_bandwidth_test(&mut self) {
        self.last_error = None;
        self.mode = AppMode::BandwidthTesting(BandwidthProgress::Downloading {
            current_mbps: 0.0,
            progress_pct: 0.0,
        });
        self.active_widget = ActiveWidget::Bandwidth;
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
            if p > max {
                max = p;
            }
        }
        for &(_, p) in &self.tcp_data {
            if p > max {
                max = p;
            }
        }
        self.chart_max_ping = max;
    }

    pub fn handle_event(
        &mut self,
        event: TelemetryEvent,
        _cmd_tx: &mpsc::Sender<EngineCommand>,
    ) -> RenderAction {
        match event {
            TelemetryEvent::Ping {
                sequence_number,
                target_ip,
                result,
                timestamp,
            } => {
                if self.current_target_ip != target_ip {
                    self.current_target_ip = target_ip.clone();
                }
                if sequence_number > self.latest_sequence {
                    self.latest_sequence = sequence_number;
                }

                self.icmp_stats.update(&result);

                // A failed sample is not drawn: plotting it as 0.0 ms (or 0 jitter) would
                // look like the best reply of the session. The latency chart leaves a gap
                // at that sequence; the sparkline cannot draw gaps, so it just skips it.
                if let Ok(ping) = result {
                    Self::push_chart_data(&mut self.icmp_data, (sequence_number as f64, ping));
                    let j = self.icmp_stats.current_jitter.round() as u64;
                    Self::push_chart_data(&mut self.jitter_history, j);

                    if ping > self.chart_max_ping {
                        self.chart_max_ping = ping;
                    }
                }

                // If this packet fell out of the window and it was the max, recompute
                if self.icmp_data.len() == HISTORY_SIZE {
                    self.update_chart_max_ping();
                }

                let metric = self.latest_metric.get_or_insert_with(|| PingMetrics {
                    sequence_number,
                    target_ip: String::new(),
                    icmp_ping: None,
                    tcp_ping: None,
                    timestamp,
                });
                metric.sequence_number = sequence_number;
                metric.target_ip = target_ip;
                metric.icmp_ping = Some(result);
                metric.timestamp = timestamp;

                RenderAction::Redraw
            }
            TelemetryEvent::Tcp {
                sequence_number,
                target_ip,
                result,
                timestamp,
            } => {
                if self.current_target_ip != target_ip {
                    self.current_target_ip = target_ip.clone();
                }
                if sequence_number > self.latest_sequence {
                    self.latest_sequence = sequence_number;
                }

                self.tcp_stats.update(&result);

                // Same as ICMP: a failed handshake leaves a gap instead of a 0.0 ms point.
                if let Ok(ping) = result {
                    Self::push_chart_data(&mut self.tcp_data, (sequence_number as f64, ping));
                    if ping > self.chart_max_ping {
                        self.chart_max_ping = ping;
                    }
                }

                if self.tcp_data.len() == HISTORY_SIZE {
                    self.update_chart_max_ping();
                }

                let metric = self.latest_metric.get_or_insert_with(|| PingMetrics {
                    sequence_number,
                    target_ip: String::new(),
                    icmp_ping: None,
                    tcp_ping: None,
                    timestamp,
                });
                metric.sequence_number = sequence_number;
                metric.target_ip = target_ip;
                metric.tcp_ping = Some(result);
                metric.timestamp = timestamp;

                RenderAction::Redraw
            }
            TelemetryEvent::Bandwidth(progress) => {
                // A frame from a test the user already cancelled (Esc) can still be in the
                // channel; it must not pull the UI back out of Ping/Traceroute/Error mode.
                // `begin_bandwidth_test` enters the mode when the user presses 's'.
                if !matches!(self.mode, AppMode::BandwidthTesting(_)) {
                    return RenderAction::Skip;
                }
                self.last_error = None;
                if let BandwidthProgress::Finished {
                    download_mbps,
                    upload_mbps,
                } = progress
                {
                    self.last_speed_test = Some((download_mbps, upload_mbps));
                }
                self.mode = AppMode::BandwidthTesting(progress);
                self.active_widget = ActiveWidget::Bandwidth;
                RenderAction::Redraw
            }
            TelemetryEvent::BandwidthError(err) => {
                if matches!(self.mode, AppMode::BandwidthTesting(_)) {
                    self.mode =
                        AppMode::BandwidthTesting(BandwidthProgress::Failed(err.to_string()));
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
                    && !self.dns_status.contains_key(ip)
                {
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
    // Only the Unix sudo worker needs to send telemetry from the UI side.
    #[cfg_attr(windows, allow(unused_variables))] tx: mpsc::Sender<TelemetryEvent>,
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
                        if let Err(e) =
                            cmd_tx.try_send(intqual_core::engine::core_engine::EngineCommand::Stop)
                            && matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_))
                        {
                            break;
                        }
                        break;
                    } else if key.code == KeyCode::Char('s') {
                        if !matches!(app.mode, AppMode::BandwidthTesting(_)) {
                            app.last_error = None;
                            match cmd_tx.try_send(EngineCommand::StartBandwidthTest) {
                                Ok(()) => {
                                    app.begin_bandwidth_test();
                                    terminal.draw(|frame| draw_ui(frame, &app))?;
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                                Err(e) => {
                                    tracing::error!("Failed to send EngineCommand: {}", e);
                                }
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

                            let _ = cmd_tx
                                .try_send(intqual_core::engine::core_engine::EngineCommand::Pause);

                            // Windows uses the ICMP Helper API, which needs no elevation,
                            // so the engine runs the traceroute in-process there.
                            #[cfg(windows)]
                            if let Err(e) = cmd_tx.try_send(EngineCommand::StartTraceroute(
                                app.current_target_ip.clone(),
                            )) {
                                if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                    break;
                                }
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                            #[cfg(not(windows))]
                            start_traceroute_worker(
                                &mut terminal,
                                app.current_target_ip.clone(),
                                tx.clone(),
                            );
                        }
                    } else if key.code == KeyCode::Esc {
                        if matches!(
                            app.mode,
                            AppMode::BandwidthTesting(_) | AppMode::Traceroute | AppMode::Error(_)
                        ) {
                            app.reset_ping_state();
                            app.mode = AppMode::Ping;
                            app.active_widget = ActiveWidget::Latency;
                            app.last_error = None;
                            if let Err(e) = cmd_tx
                                .try_send(intqual_core::engine::core_engine::EngineCommand::Resume)
                            {
                                if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                    break;
                                }
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Enter
                        && matches!(
                            app.mode,
                            AppMode::BandwidthTesting(BandwidthProgress::Finished { .. })
                                | AppMode::BandwidthTesting(BandwidthProgress::Failed(_))
                                | AppMode::Traceroute
                                | AppMode::Error(_)
                        )
                    {
                        app.reset_ping_state();
                        app.mode = AppMode::Ping;
                        app.active_widget = ActiveWidget::Latency;
                        app.last_error = None;
                        if let Err(e) = cmd_tx
                            .try_send(intqual_core::engine::core_engine::EngineCommand::Resume)
                        {
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

    let is_testing = matches!(
        app.mode,
        AppMode::BandwidthTesting(
            BandwidthProgress::Downloading { .. } | BandwidthProgress::Uploading { .. }
        )
    );
    let is_finished_or_failed = matches!(
        app.mode,
        AppMode::BandwidthTesting(
            BandwidthProgress::Finished { .. } | BandwidthProgress::Failed(_)
        )
    );
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

/// Runs traceroute in a `sudo` child process (`--worker-mode traceroute`) and forwards
/// its JSON-line hops to the UI. Raw ICMP needs root on Unix, and the worker keeps the
/// TUI itself unprivileged.
#[cfg(not(windows))]
fn start_traceroute_worker(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    target_ip: String,
    tx_worker: mpsc::Sender<TelemetryEvent>,
) {
    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);

    let exe_path = std::env::current_exe().unwrap_or_else(|_| "intqual".into());

    // Synchronously authenticate via sudo to use standard terminal prompts
    let _ = std::process::Command::new("sudo").arg("-v").status();

    let _ = stdout().execute(EnterAlternateScreen);
    let _ = enable_raw_mode();
    let _ = terminal.clear();

    tokio::runtime::Handle::current().spawn(async move {
        use intqual_core::models::ProbeError;
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut child = match tokio::process::Command::new("sudo")
            .arg("-E")
            .arg(exe_path)
            .arg("--worker-mode")
            .arg("traceroute")
            .arg(&target_ip)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx_worker
                    .send(TelemetryEvent::TracerouteError(ProbeError::IpcError(
                        format!("Failed to spawn worker: {}", e),
                    )))
                    .await;
                return;
            }
        };

        let stdout_pipe = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = tx_worker
                    .send(TelemetryEvent::TracerouteError(ProbeError::IpcError(
                        "Failed to capture stdout".to_string(),
                    )))
                    .await;
                return;
            }
        };

        let mut reader = BufReader::new(stdout_pipe).lines();

        while let Ok(Some(line)) = reader.next_line().await {
            match serde_json::from_str::<intqual_core::models::TracerouteHop>(&line) {
                Ok(hop) => {
                    if let Some(ip) = &hop.ip_address {
                        let ip_clone = ip.clone();
                        let tx_dns = tx_worker.clone();
                        tokio::spawn(async move {
                            let ip_for_closure = ip_clone.clone();
                            let hostname = tokio::task::spawn_blocking(move || {
                                match ip_for_closure.parse::<std::net::IpAddr>() {
                                    Ok(addr) => dns_lookup::lookup_addr(&addr).ok(),
                                    Err(_) => None,
                                }
                            })
                            .await
                            .unwrap_or(None);

                            let _ = tx_dns
                                .send(TelemetryEvent::DnsResolved {
                                    ip: ip_clone,
                                    hostname,
                                })
                                .await;
                        });
                    }
                    let _ = tx_worker.send(TelemetryEvent::TracerouteHop(hop)).await;
                }
                Err(e) => {
                    let _ = tx_worker
                        .send(TelemetryEvent::TracerouteError(ProbeError::IpcError(
                            format!("Parse error: {} for line: {}", e, line),
                        )))
                        .await;
                }
            }
        }

        match child.wait().await {
            Ok(status) if status.success() => {
                let _ = tx_worker.send(TelemetryEvent::TracerouteComplete).await;
            }
            Ok(status) => {
                let mut err_msg = format!("Worker exited with status: {}", status);
                if let Some(mut stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let mut s = String::new();
                    if stderr.read_to_string(&mut s).await.is_ok() && !s.is_empty() {
                        err_msg.push_str(&format!(" - Stderr: {}", s));
                    }
                }
                let _ = tx_worker
                    .send(TelemetryEvent::TracerouteError(ProbeError::IpcError(
                        err_msg,
                    )))
                    .await;
            }
            Err(e) => {
                let _ = tx_worker
                    .send(TelemetryEvent::TracerouteError(ProbeError::IpcError(
                        format!("Failed to wait on worker: {}", e),
                    )))
                    .await;
            }
        }
    });
}
