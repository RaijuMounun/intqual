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

#[cfg(test)]
mod tests {
    use super::*;
    use intqual_core::models::TracerouteHop;

    const TARGET: &str = "192.0.2.1";

    fn refused() -> ProbeError {
        ProbeError::Socket(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn ping(seq: u64, result: std::result::Result<f64, ProbeError>) -> TelemetryEvent {
        TelemetryEvent::Ping {
            sequence_number: seq,
            target_ip: TARGET.to_string(),
            result,
            timestamp: 1_000 + seq,
        }
    }

    fn tcp(seq: u64, result: std::result::Result<f64, ProbeError>) -> TelemetryEvent {
        TelemetryEvent::Tcp {
            sequence_number: seq,
            target_ip: TARGET.to_string(),
            result,
            timestamp: 1_000 + seq,
        }
    }

    fn hop(n: u8, ip: Option<&str>) -> TelemetryEvent {
        TelemetryEvent::TracerouteHop(TracerouteHop {
            hop_number: n,
            ip_address: ip.map(str::to_string),
            avg_rtt_ms: Some(1.0),
            is_destination: false,
            icmp_error: None,
        })
    }

    /// Handles one event. `handle_event` never sends on the command channel; the sender
    /// only satisfies the signature.
    fn handle(app: &mut AppState, event: TelemetryEvent) -> RenderAction {
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        app.handle_event(event, &cmd_tx)
    }

    fn feed_into(app: &mut AppState, events: impl IntoIterator<Item = TelemetryEvent>) {
        for event in events {
            let _ = handle(app, event);
        }
    }

    /// Feeds events into a fresh `AppState` and returns it.
    fn feed(events: impl IntoIterator<Item = TelemetryEvent>) -> AppState {
        let mut app = AppState::new();
        feed_into(&mut app, events);
        app
    }

    fn stats_from(results: Vec<std::result::Result<f64, ProbeError>>) -> NetworkStats {
        let mut stats = NetworkStats::default();
        for r in &results {
            stats.update(r);
        }
        stats
    }

    // ---- NetworkStats::update ----

    #[test]
    fn first_success_sets_min_max_and_no_jitter() {
        let s = stats_from(vec![Ok(12.0)]);
        assert_eq!(s.min_ping, 12.0);
        assert_eq!(s.max_ping, 12.0);
        assert_eq!(s.current_jitter, 0.0);
        assert_eq!(s.jitter_count, 0);
        assert_eq!(s.last_ping, Some(12.0));
        assert_eq!(s.loss_pct, 0.0);
    }

    #[test]
    fn min_and_max_track_extremes() {
        let s = stats_from(vec![Ok(20.0), Ok(10.0), Ok(30.0), Ok(15.0)]);
        assert_eq!(s.min_ping, 10.0);
        assert_eq!(s.max_ping, 30.0);
    }

    /// `min_ping == 0.0` doubles as "no sample yet", so a genuine 0.0 ms sample is
    /// overwritten by the next one. Pinned here so a change to the sentinel is deliberate.
    #[test]
    fn min_ping_treats_zero_as_unset() {
        let s = stats_from(vec![Ok(0.0), Ok(5.0)]);
        assert_eq!(s.min_ping, 5.0);
    }

    #[test]
    fn jitter_is_mean_absolute_difference_of_consecutive_samples() {
        let s = stats_from(vec![Ok(10.0), Ok(30.0), Ok(20.0)]);
        assert_eq!(s.current_jitter, 10.0);
        assert_eq!(s.jitter_count, 2);
        assert_eq!(s.jitter_sum, 30.0);
        assert_eq!(s.avg_jitter, 15.0);
    }

    /// RFC 3393: jitter only between consecutive probes that both succeeded.
    #[test]
    fn loss_breaks_the_jitter_chain() {
        let s = stats_from(vec![Ok(10.0), Err(ProbeError::IcmpTimeout), Ok(30.0)]);
        assert_eq!(s.current_jitter, 0.0);
        assert_eq!(s.jitter_count, 0);

        let s = stats_from(vec![
            Ok(10.0),
            Err(ProbeError::IcmpTimeout),
            Ok(30.0),
            Ok(35.0),
        ]);
        assert_eq!(s.current_jitter, 5.0);
        assert_eq!(s.jitter_count, 1);
        assert_eq!(s.avg_jitter, 5.0);
    }

    #[test]
    fn non_loss_error_also_breaks_the_jitter_chain() {
        let s = stats_from(vec![Ok(10.0), Err(refused()), Ok(30.0)]);
        assert_eq!(s.jitter_count, 0);
        assert_eq!(s.last_ping, Some(30.0));
    }

    #[test]
    fn loss_leaves_min_and_max_untouched() {
        let s = stats_from(vec![Ok(10.0), Err(ProbeError::TcpTimeout)]);
        assert_eq!(s.min_ping, 10.0);
        assert_eq!(s.max_ping, 10.0);
        assert_eq!(s.last_ping, None);
    }

    #[test]
    fn timeouts_count_as_loss() {
        let s = stats_from(vec![
            Ok(10.0),
            Err(ProbeError::IcmpTimeout),
            Err(ProbeError::TcpTimeout),
            Ok(10.0),
        ]);
        assert_eq!(s.total_count, 4);
        assert_eq!(s.loss_count, 2);
        assert_eq!(s.loss_pct, 50.0);
    }

    #[test]
    fn icmp_error_reply_counts_as_loss() {
        let s = stats_from(vec![
            Ok(10.0),
            Ok(10.0),
            Err(ProbeError::IcmpError { type_: 3, code: 1 }),
        ]);
        assert_eq!(s.total_count, 3);
        assert_eq!(s.loss_count, 1);
        assert_eq!(s.error_count, 0);
        assert!(approx(s.loss_pct, 100.0 / 3.0));
    }

    /// A refused connection means the target answered (RST): not loss, but not hidden.
    #[test]
    fn refused_connection_is_an_error_not_loss() {
        let s = stats_from(vec![Ok(10.0), Err(refused()), Err(ProbeError::TcpTimeout)]);
        assert_eq!(s.total_count, 2);
        assert_eq!(s.loss_count, 1);
        assert_eq!(s.error_count, 1);
        assert_eq!(s.loss_pct, 50.0);
    }

    #[test]
    fn permission_denied_is_an_error_not_loss() {
        let s = stats_from(vec![
            Ok(10.0),
            Err(ProbeError::PermissionDenied),
            Err(ProbeError::IcmpTimeout),
        ]);
        assert_eq!(s.total_count, 2);
        assert_eq!(s.loss_count, 1);
        assert_eq!(s.error_count, 1);
        assert_eq!(s.loss_pct, 50.0);
    }

    #[test]
    fn every_other_error_variant_is_an_error_not_loss() {
        let others = vec![
            Err(ProbeError::DnsResolution("x".into())),
            Err(ProbeError::BandwidthTestFailed("x".into())),
            Err(ProbeError::RateLimited("x".into())),
            Err(ProbeError::TimeSyncError),
            Err(ProbeError::ThreadPanic("x".into())),
            Err(ProbeError::PacketError("x".into())),
            Err(ProbeError::IpcError("x".into())),
        ];
        let n = others.len() as u64;
        let s = stats_from(others);
        assert_eq!(s.total_count, 0);
        assert_eq!(s.loss_count, 0);
        assert_eq!(s.error_count, n);
        assert_eq!(s.loss_pct, 0.0);
    }

    #[test]
    fn all_lost_is_one_hundred_percent() {
        let s = stats_from(vec![
            Err(ProbeError::IcmpTimeout),
            Err(ProbeError::IcmpError { type_: 3, code: 0 }),
        ]);
        assert_eq!(s.loss_pct, 100.0);
    }

    // ---- AppState defaults ----

    #[test]
    fn new_state_starts_in_ping_mode_with_no_data() {
        let app = AppState::new();
        assert!(matches!(app.mode, AppMode::Ping));
        assert_eq!(app.active_widget, ActiveWidget::Latency);
        assert_eq!(app.latest_sequence, 0);
        assert_eq!(app.chart_max_ping, 50.0);
        assert!(app.latest_metric.is_none());
        assert!(app.icmp_data.is_empty() && app.tcp_data.is_empty());
        assert!(app.jitter_history.is_empty());
        assert!(app.last_error.is_none() && app.last_speed_test.is_none());
        assert!(app.traceroute_hops.is_empty() && app.dns_status.is_empty());
        assert!(!app.traceroute_complete);
        assert!(app.current_target_ip.is_empty());
    }

    // ---- handle_event: Ping / Tcp ----

    #[test]
    fn ping_success_updates_stats_chart_and_metric() {
        let mut app = AppState::new();
        let action = handle(&mut app, ping(1, Ok(12.5)));

        assert!(matches!(action, RenderAction::Redraw));
        assert_eq!(app.latest_sequence, 1);
        assert_eq!(app.current_target_ip, TARGET);
        assert_eq!(app.icmp_stats.total_count, 1);
        assert_eq!(app.icmp_data, VecDeque::from([(1.0, 12.5)]));
        assert_eq!(app.jitter_history, VecDeque::from([0]));
        assert_eq!(app.tcp_stats.total_count, 0);

        let metric = app.latest_metric.as_ref().expect("metric after first ping");
        assert_eq!(metric.sequence_number, 1);
        assert_eq!(metric.target_ip, TARGET);
        assert_eq!(metric.timestamp, 1_001);
        assert!(matches!(metric.icmp_ping, Some(Ok(p)) if p == 12.5));
    }

    #[test]
    fn first_ping_does_not_invent_a_tcp_result() {
        let app = feed([ping(1, Ok(10.0))]);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(metric.tcp_ping.is_none());
    }

    #[test]
    fn first_tcp_does_not_invent_an_icmp_result() {
        let app = feed([tcp(1, Ok(20.0))]);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(metric.icmp_ping.is_none());
        assert!(matches!(metric.tcp_ping, Some(Ok(p)) if p == 20.0));
        assert_eq!(app.tcp_data, VecDeque::from([(1.0, 20.0)]));
        assert!(app.icmp_data.is_empty());
        assert!(app.jitter_history.is_empty());
    }

    #[test]
    fn ping_and_tcp_results_are_kept_side_by_side() {
        let app = feed([ping(1, Ok(10.0)), tcp(1, Err(refused()))]);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(matches!(metric.icmp_ping, Some(Ok(_))));
        assert!(matches!(metric.tcp_ping, Some(Err(ProbeError::Socket(_)))));
        assert_eq!(app.tcp_stats.error_count, 1);
    }

    #[test]
    fn tcp_then_ping_keeps_both_results() {
        let mut app = feed([tcp(7, Ok(20.0))]);
        {
            let metric = app.latest_metric.as_ref().expect("metric");
            assert_eq!(metric.target_ip, TARGET);
            assert_eq!(metric.timestamp, 1_007);
        }
        feed_into(&mut app, [ping(8, Ok(10.0))]);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(matches!(metric.tcp_ping, Some(Ok(p)) if p == 20.0));
        assert!(matches!(metric.icmp_ping, Some(Ok(p)) if p == 10.0));
        assert_eq!(metric.sequence_number, 8);
    }

    #[test]
    fn non_loss_error_is_shown_but_not_plotted_or_counted_as_sample() {
        let app = feed([ping(1, Err(ProbeError::PermissionDenied))]);
        assert!(app.icmp_data.is_empty());
        assert!(app.jitter_history.is_empty());
        assert_eq!(app.icmp_stats.total_count, 0);
        assert_eq!(app.icmp_stats.error_count, 1);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(matches!(
            metric.icmp_ping,
            Some(Err(ProbeError::PermissionDenied))
        ));
    }

    #[test]
    fn lost_ping_is_counted_but_not_plotted() {
        let app = feed([ping(1, Ok(10.0)), ping(2, Err(ProbeError::IcmpTimeout))]);
        assert_eq!(app.icmp_data, VecDeque::from([(1.0, 10.0)]));
        // No 0 is pushed to the sparkline for a lost packet.
        assert_eq!(app.jitter_history, VecDeque::from([0]));
        assert_eq!(app.icmp_stats.loss_count, 1);
        assert_eq!(app.latest_sequence, 2);
        let metric = app.latest_metric.as_ref().expect("metric");
        assert!(matches!(
            metric.icmp_ping,
            Some(Err(ProbeError::IcmpTimeout))
        ));
    }

    #[test]
    fn lost_tcp_is_counted_but_not_plotted() {
        let app = feed([tcp(1, Err(ProbeError::TcpTimeout)), tcp(2, Ok(30.0))]);
        assert_eq!(app.tcp_data, VecDeque::from([(2.0, 30.0)]));
        assert_eq!(app.tcp_stats.loss_count, 1);
    }

    #[test]
    fn jitter_history_records_rounded_current_jitter() {
        let app = feed([ping(1, Ok(10.0)), ping(2, Ok(13.6))]);
        assert_eq!(app.jitter_history, VecDeque::from([0, 4]));
    }

    #[test]
    fn jitter_history_restarts_after_a_loss() {
        let app = feed([
            ping(1, Ok(10.0)),
            ping(2, Err(ProbeError::IcmpTimeout)),
            ping(3, Ok(30.0)),
            ping(4, Ok(35.0)),
        ]);
        // 3 has no successful predecessor, so its jitter is 0, not |30 - 10|.
        assert_eq!(app.jitter_history, VecDeque::from([0, 0, 5]));
        assert_eq!(app.icmp_data.len(), 3);
    }

    #[test]
    fn latest_sequence_never_moves_backwards() {
        let app = feed([ping(5, Ok(10.0)), ping(3, Ok(10.0)), tcp(4, Ok(10.0))]);
        assert_eq!(app.latest_sequence, 5);
        // The metric shows the most recently *received* sample, not the highest sequence.
        assert_eq!(
            app.latest_metric.as_ref().map(|m| m.sequence_number),
            Some(4)
        );
    }

    #[test]
    fn target_ip_follows_the_latest_event() {
        let mut app = feed([ping(1, Ok(10.0))]);
        feed_into(
            &mut app,
            [TelemetryEvent::Ping {
                sequence_number: 2,
                target_ip: "198.51.100.7".into(),
                result: Ok(10.0),
                timestamp: 0,
            }],
        );
        assert_eq!(app.current_target_ip, "198.51.100.7");
    }

    #[test]
    fn chart_history_is_capped_at_history_size() {
        let n = HISTORY_SIZE as u64 * 3;
        let app = feed((1..=n).flat_map(|i| [ping(i, Ok(10.0)), tcp(i, Ok(20.0))]));
        assert_eq!(app.icmp_data.len(), HISTORY_SIZE);
        assert_eq!(app.tcp_data.len(), HISTORY_SIZE);
        assert_eq!(app.jitter_history.len(), HISTORY_SIZE);
        // Oldest points are dropped first.
        let first_kept = (n - HISTORY_SIZE as u64 + 1) as f64;
        assert_eq!(app.icmp_data.front().map(|p| p.0), Some(first_kept));
        assert_eq!(app.icmp_data.back().map(|p| p.0), Some(n as f64));
    }

    /// Widgets read `as_slices().0`, which only sees every point if the deque is
    /// contiguous. Whether the ring has wrapped depends on capacity and push count, so
    /// check after every push across several full windows.
    #[test]
    fn chart_history_stays_contiguous_for_the_widgets() {
        let mut app = AppState::new();
        for i in 1..=HISTORY_SIZE as u64 * 3 {
            feed_into(&mut app, [ping(i, Ok(10.0)), tcp(i, Ok(20.0))]);
            assert!(
                app.icmp_data.as_slices().1.is_empty(),
                "icmp wrapped at {i}"
            );
            assert!(app.tcp_data.as_slices().1.is_empty(), "tcp wrapped at {i}");
            assert!(
                app.jitter_history.as_slices().1.is_empty(),
                "jitter wrapped at {i}"
            );
        }
    }

    #[test]
    fn chart_max_has_a_floor_of_fifty() {
        let app = feed([ping(1, Ok(10.0)), tcp(1, Ok(20.0))]);
        assert_eq!(app.chart_max_ping, 50.0);
    }

    #[test]
    fn chart_max_grows_with_icmp_or_tcp_spikes() {
        let mut app = feed([ping(1, Ok(120.0))]);
        assert_eq!(app.chart_max_ping, 120.0);
        feed_into(&mut app, [tcp(1, Ok(300.0))]);
        assert_eq!(app.chart_max_ping, 300.0);
    }

    #[test]
    fn chart_max_shrinks_once_the_spike_leaves_the_window() {
        let mut app = feed([ping(1, Ok(400.0))]);
        feed_into(
            &mut app,
            (2..=HISTORY_SIZE as u64).map(|i| ping(i, Ok(10.0))),
        );
        // The spike is still the oldest of HISTORY_SIZE points.
        assert_eq!(app.chart_max_ping, 400.0);

        feed_into(&mut app, [ping(HISTORY_SIZE as u64 + 1, Ok(10.0))]);
        assert_eq!(app.chart_max_ping, 50.0);
    }

    #[test]
    fn chart_max_shrinks_for_tcp_too() {
        let mut app = feed([tcp(1, Ok(400.0))]);
        feed_into(
            &mut app,
            (2..=HISTORY_SIZE as u64 + 1).map(|i| tcp(i, Ok(10.0))),
        );
        assert_eq!(app.chart_max_ping, 50.0);
    }

    // ---- handle_event: bandwidth ----

    fn downloading(pct: f64) -> TelemetryEvent {
        TelemetryEvent::Bandwidth(BandwidthProgress::Downloading {
            current_mbps: 50.0,
            progress_pct: pct,
        })
    }

    #[test]
    fn begin_bandwidth_test_enters_testing_mode() {
        let mut app = AppState::new();
        app.last_error = Some("old".into());
        app.begin_bandwidth_test();
        assert!(matches!(
            app.mode,
            AppMode::BandwidthTesting(BandwidthProgress::Downloading { .. })
        ));
        assert_eq!(app.active_widget, ActiveWidget::Bandwidth);
        assert!(app.last_error.is_none());
    }

    #[test]
    fn bandwidth_progress_updates_a_running_test() {
        let mut app = AppState::new();
        app.begin_bandwidth_test();
        app.last_error = Some("stale".into());

        let action = handle(&mut app, downloading(40.0));
        assert!(matches!(action, RenderAction::Redraw));
        assert!(matches!(
            app.mode,
            AppMode::BandwidthTesting(BandwidthProgress::Downloading { progress_pct, .. })
                if progress_pct == 40.0
        ));
        assert!(app.last_error.is_none());
        assert!(app.last_speed_test.is_none());
    }

    #[test]
    fn bandwidth_finished_records_the_result() {
        let mut app = AppState::new();
        app.begin_bandwidth_test();
        let _ = handle(
            &mut app,
            TelemetryEvent::Bandwidth(BandwidthProgress::Finished {
                download_mbps: 95.0,
                upload_mbps: 20.0,
            }),
        );
        assert_eq!(app.last_speed_test, Some((95.0, 20.0)));
        assert!(matches!(
            app.mode,
            AppMode::BandwidthTesting(BandwidthProgress::Finished { .. })
        ));
    }

    #[test]
    fn late_bandwidth_event_is_ignored_in_ping_mode() {
        let mut app = AppState::new();
        let action = handle(&mut app, downloading(80.0));
        assert!(matches!(action, RenderAction::Skip));
        assert!(matches!(app.mode, AppMode::Ping));
        assert_eq!(app.active_widget, ActiveWidget::Latency);
    }

    /// After Esc, a stale `Finished` frame must not overwrite the last good result
    /// or pull the UI back to the speed-test screen.
    #[test]
    fn late_finished_frame_is_skipped_outside_a_test() {
        let modes = [
            (AppMode::Ping, ActiveWidget::Latency),
            (AppMode::Traceroute, ActiveWidget::Traceroute),
            (AppMode::Error("x".into()), ActiveWidget::Error),
        ];
        for (mode, widget) in modes {
            let mut app = AppState::new();
            app.mode = mode;
            app.active_widget = widget;
            app.last_speed_test = Some((1.0, 2.0));
            app.last_error = Some("kept".into());

            let action = handle(
                &mut app,
                TelemetryEvent::Bandwidth(BandwidthProgress::Finished {
                    download_mbps: 95.0,
                    upload_mbps: 20.0,
                }),
            );
            assert!(matches!(action, RenderAction::Skip));
            assert!(!matches!(app.mode, AppMode::BandwidthTesting(_)));
            assert_eq!(app.active_widget, widget);
            assert_eq!(app.last_speed_test, Some((1.0, 2.0)));
            assert_eq!(app.last_error.as_deref(), Some("kept"));
        }
    }

    #[test]
    fn bandwidth_error_during_a_test_marks_it_failed() {
        let mut app = AppState::new();
        app.begin_bandwidth_test();
        let action = handle(
            &mut app,
            TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed("HTTP 503".into())),
        );
        assert!(matches!(action, RenderAction::Redraw));
        assert!(matches!(
            &app.mode,
            AppMode::BandwidthTesting(BandwidthProgress::Failed(msg)) if msg.contains("HTTP 503")
        ));
    }

    #[test]
    fn bandwidth_error_outside_a_test_is_ignored() {
        let mut app = AppState::new();
        let action = handle(
            &mut app,
            TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed("late".into())),
        );
        assert!(matches!(action, RenderAction::Skip));
        assert!(matches!(app.mode, AppMode::Ping));
    }

    // ---- handle_event: fatal / traceroute / DNS ----

    #[test]
    fn fatal_switches_to_error_mode() {
        let mut app = AppState::new();
        let action = handle(
            &mut app,
            TelemetryEvent::Fatal(ProbeError::DnsResolution("no such host".into())),
        );
        assert!(matches!(action, RenderAction::Redraw));
        assert!(matches!(&app.mode, AppMode::Error(msg) if msg.contains("no such host")));
        assert_eq!(app.active_widget, ActiveWidget::Error);
        let err = app.last_error.as_deref().expect("last_error set");
        assert!(err.starts_with("FATAL ERROR: "));
        assert!(err.contains("no such host"));
    }

    #[test]
    fn traceroute_hop_is_stored_and_its_ip_queued_for_dns() {
        let mut app = AppState::new();
        let action = handle(&mut app, hop(1, Some("10.0.0.1")));
        assert!(matches!(action, RenderAction::Redraw));
        assert_eq!(app.traceroute_hops.len(), 1);
        assert!(matches!(
            app.dns_status.get("10.0.0.1"),
            Some(DnsStatus::Resolving)
        ));
    }

    #[test]
    fn silent_hop_adds_no_dns_entry() {
        let app = feed([hop(1, None)]);
        assert_eq!(app.traceroute_hops.len(), 1);
        assert!(app.dns_status.is_empty());
    }

    #[test]
    fn repeated_hop_ip_keeps_its_resolved_name() {
        let app = feed([
            hop(1, Some("10.0.0.1")),
            TelemetryEvent::DnsResolved {
                ip: "10.0.0.1".into(),
                hostname: Some("gw.lan".into()),
            },
            hop(2, Some("10.0.0.1")),
        ]);
        assert_eq!(app.traceroute_hops.len(), 2);
        assert!(matches!(
            app.dns_status.get("10.0.0.1"),
            Some(DnsStatus::Resolved(name)) if name == "gw.lan"
        ));
    }

    #[test]
    fn failed_reverse_lookup_is_marked_failed() {
        let app = feed([
            hop(1, Some("10.0.0.1")),
            TelemetryEvent::DnsResolved {
                ip: "10.0.0.1".into(),
                hostname: None,
            },
        ]);
        assert!(matches!(
            app.dns_status.get("10.0.0.1"),
            Some(DnsStatus::Failed)
        ));
    }

    #[test]
    fn traceroute_complete_sets_the_flag() {
        let mut app = AppState::new();
        let action = handle(&mut app, TelemetryEvent::TracerouteComplete);
        assert!(matches!(action, RenderAction::Redraw));
        assert!(app.traceroute_complete);
    }

    #[test]
    fn traceroute_error_is_shown_as_last_error() {
        let mut app = AppState::new();
        let action = handle(
            &mut app,
            TelemetryEvent::TracerouteError(ProbeError::IpcError("worker died".into())),
        );
        assert!(matches!(action, RenderAction::Redraw));
        let err = app.last_error.as_deref().expect("last_error set");
        assert!(err.starts_with("Traceroute Error: "));
        assert!(err.contains("worker died"));
    }

    // ---- reset_ping_state ----

    #[test]
    fn reset_ping_state_clears_ping_data_only() {
        let mut app = feed([
            ping(1, Ok(400.0)),
            ping(2, Err(ProbeError::IcmpTimeout)),
            tcp(1, Ok(20.0)),
            hop(1, Some("10.0.0.1")),
        ]);
        app.last_speed_test = Some((95.0, 20.0));
        app.reset_ping_state();

        for stats in [&app.icmp_stats, &app.tcp_stats] {
            assert_eq!(stats.total_count, 0);
            assert_eq!(stats.loss_count, 0);
            assert_eq!(stats.error_count, 0);
            assert_eq!(stats.min_ping, 0.0);
            assert_eq!(stats.max_ping, 0.0);
            assert_eq!(stats.last_ping, None);
        }
        assert!(app.icmp_data.is_empty() && app.tcp_data.is_empty());
        assert!(app.jitter_history.is_empty());
        assert_eq!(app.chart_max_ping, 50.0);
        assert_eq!(app.latest_sequence, 0);
        assert!(app.latest_metric.is_none());

        // Not ping state: kept for the next screen.
        assert_eq!(app.last_speed_test, Some((95.0, 20.0)));
        assert_eq!(app.current_target_ip, TARGET);
        assert_eq!(app.traceroute_hops.len(), 1);
    }
}
