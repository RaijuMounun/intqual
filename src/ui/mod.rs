use crate::engine::core_engine::EngineCommand;
use crate::models::{PingMetrics, TelemetryEvent, BandwidthProgress, ProbeError};
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    layout::Alignment,
    prelude::*,
    symbols,
    widgets::{
        Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, LegendPosition, Paragraph,
        Sparkline,
    },
};
use std::io::{Result, stdout};
use std::time::Duration;
use tokio::sync::mpsc;
use tui_big_text::{BigText, PixelSize};

/// Defines the maximum number of data points retained in memory for rendering.
/// 100 perfectly balances the memory footprint with optimal visual data density
/// for standard terminal widths, ensuring the sliding window remains legible.
const HISTORY_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub enum AppMode {
    Ping,
    BandwidthTesting(BandwidthProgress),
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
}

/// Manages the volatile view state for the terminal UI.
/// By isolating the view state (history, sequence tracking) from the CoreEngine,
/// the engine remains completely stateless and pure, avoiding complex lock-contention
/// between the async I/O reactor and the synchronous rendering thread.
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
        }
    }

    /// Ingests a new metric into the ring buffer.
    pub fn push_metric(&mut self, metric: PingMetrics) {
        if metric.sequence_number > self.latest_sequence {
            self.latest_sequence = metric.sequence_number;
        }
        let index = (metric.sequence_number % HISTORY_SIZE as u64) as usize;
        self.history[index] = Some(metric);
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
                TelemetryEvent::Ping(metric) => {
                    app.push_metric(metric);
                    state_changed = true;
                }
                TelemetryEvent::Bandwidth(progress) => {
                    app.last_error = None;
                    if let BandwidthProgress::Finished { download_mbps, upload_mbps } = &progress {
                        app.last_speed_test = Some((*download_mbps, *upload_mbps));
                    }
                    app.mode = AppMode::BandwidthTesting(progress);
                    state_changed = true;
                }
                TelemetryEvent::BandwidthError(err) => {
                    if let AppMode::Ping = app.mode {
                        // Ignore stale errors if already manually aborted
                    } else {
                        app.mode = AppMode::Ping;
                        app.last_error = Some(err.to_string());
                        if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Resume) {
                            tracing::error!("Failed to send EngineCommand: {}", e);
                        }
                        state_changed = true;
                    }
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
                    } else if key.code == KeyCode::Esc {
                        if matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Downloading {..} | BandwidthProgress::Uploading {..})) {
                            app.mode = AppMode::Ping;
                            app.last_error = None;
                            if let Err(e) = cmd_tx.try_send(crate::engine::core_engine::EngineCommand::Resume) {
                                tracing::error!("Failed to send EngineCommand: {}", e);
                            }
                        }
                    } else if key.code == KeyCode::Enter {
                        if matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..})) {
                            app.mode = AppMode::Ping;
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

    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(screen_layout[1]);

    let is_testing = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Downloading {..} | BandwidthProgress::Uploading {..}));
    let is_finished = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..}));
    let is_testing_or_finished = is_testing || is_finished;

    let nav_text = if is_testing {
        "[Q: Quit] | [Esc: Cancel Test]"
    } else if is_finished {
        "[Q: Quit] | [Enter: Return to Ping]"
    } else {
        "[Q: Quit] | [S: Speed Test]"
    };

    let top_bar = Paragraph::new(nav_text)
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Right);

    frame.render_widget(top_bar, screen_layout[0]);

    let start_seq = app.latest_sequence.saturating_sub(HISTORY_SIZE as u64);


    // 3. LEFT COLUMN (Actionable Metrics & Alarms)
    let (loss_pct, jitter) = app.calculate_stats();
    let latest_idx = (app.latest_sequence % HISTORY_SIZE as u64) as usize;

    let mut stats_lines = Vec::new();

    if is_testing {
        stats_lines.push(Line::from(vec![Span::styled(
            "[TESTING BANDWIDTH...]",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )]));
        stats_lines.push(Line::from(""));
    } else if is_finished {
        stats_lines.push(Line::from(vec![Span::styled(
            "[TEST FINISHED]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )]));
        stats_lines.push(Line::from(""));
    }

    if app.latest_sequence == 0 {
        stats_lines.push(Line::from("  Waiting for data..."));
    } else if let Some(ref metric) = app.history[latest_idx] {
        let mut perm_denied = false;
        let (icmp_str, icmp_color_override) = match &metric.icmp_ping {
            Ok(ms) => (format!("{:.1} ms", ms), None),
            Err(ProbeError::IcmpTimeout) => ("Timeout".to_string(), Some(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
            Err(ProbeError::PermissionDenied) => {
                perm_denied = true;
                ("Perm Denied".to_string(), Some(Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)))
            },
            Err(e) => (e.to_string(), Some(Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))),
        };
        let tcp_str = match &metric.tcp_ping {
            Ok(ms) => format!("{:.1} ms", ms),
            Err(e) => e.to_string(),
        };

        let (mut icmp_color, tcp_color, jitter_style, loss_style) = if is_testing_or_finished {
            let gray = Style::default().fg(Color::DarkGray);
            (gray, gray, gray, gray)
        } else {
            let j_style = if jitter > 20.0 {
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            };

            let l_style = if loss_pct > 0.0 {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightRed)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            };

            (
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
                j_style,
                l_style,
            )
        };

        if let Some(c) = icmp_color_override {
            if !is_testing_or_finished {
                icmp_color = c;
            }
        }

        if perm_denied {
            stats_lines.push(Line::from(vec![Span::styled(
                " NO RAW SOCKET PERMISSIONS ",
                Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD),
            )]));
            stats_lines.push(Line::from(""));
        }

        stats_lines.push(Line::from(vec![Span::styled(
            " Target:",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {}", metric.target_ip),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(
            " ICMP Ping (Network):",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {}", icmp_str),
            icmp_color,
        )]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(
            " TCP Ping (App Layer):",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {}", tcp_str),
            tcp_color,
        )]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(
            " Jitter (Stability):",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {:.1} ms", jitter),
            jitter_style,
        )]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(
            " Pkt Loss (Survival):",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {:.1}%", loss_pct),
            loss_style,
        )]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(
            format!(" Seq ID: {}", metric.sequence_number),
            Style::default().fg(Color::DarkGray),
        )]));
    }

    if let Some((down, up)) = app.last_speed_test {
        stats_lines.push(Line::from(""));
        stats_lines.push(Line::from(vec![Span::styled(
            " Last Speed Test:",
            Style::default().fg(Color::DarkGray),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" Down: {:.1} Mbps", down),
            Style::default().fg(Color::LightCyan),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" Up:   {:.1} Mbps", up),
            Style::default().fg(Color::LightMagenta),
        )]));
    }

    if let Some(ref err) = app.last_error {
        stats_lines.push(Line::from(""));
        stats_lines.push(Line::from(vec![Span::styled(
            " Bandwidth Error:",
            Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
        )]));
        stats_lines.push(Line::from(vec![Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::LightRed),
        )]));
    }

    let stats_block = Paragraph::new(Text::from(stats_lines)).block(
        Block::default()
            .title(" Live Metrics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(stats_block, main_layout[0]);

    // 4 & 5. RIGHT COLUMN (Contextual Swap)
    match app.mode {
        AppMode::Ping => {
            let right_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
                .split(main_layout[1]);

            let datasets = vec![
                Dataset::default()
                    .name("TCP (App)")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(Color::DarkGray))
                    .data(&app.tcp_data),
                Dataset::default()
                    .name("ICMP (Ping)")
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(Color::LightCyan))
                    .data(&app.icmp_data),
            ];

            let x_bounds = [start_seq as f64, app.latest_sequence as f64];
            let y_bounds = [0.0, app.max_ping * 1.1];

            let chart = Chart::new(datasets)
                .block(
                    Block::default()
                        .title(" Latency History (ms) ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::LightCyan)),
                )
                .x_axis(
                    Axis::default()
                        .style(Style::default().fg(Color::DarkGray))
                        .bounds(x_bounds),
                )
                .y_axis(
                    Axis::default()
                        .title("ms")
                        .style(Style::default().fg(Color::DarkGray))
                        .bounds(y_bounds)
                        .labels(vec![
                            Span::raw("0"),
                            Span::raw(format!("{:.0}", app.max_ping / 2.0)),
                            Span::raw(format!("{:.0}", app.max_ping)),
                        ]),
                )
                .legend_position(Some(LegendPosition::TopLeft));

            frame.render_widget(chart, right_layout[0]);

            let jitter_color = if jitter > 20.0 {
                Color::LightYellow
            } else {
                Color::Magenta
            };

            let jitter_sparkline = Sparkline::default()
                .block(
                    Block::default()
                        .title(" Jitter Deviation Trend ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .data(&app.jitter_history)
                .style(Style::default().fg(jitter_color));

            frame.render_widget(jitter_sparkline, right_layout[1]);
        }
        AppMode::BandwidthTesting(ref progress) => match progress {
            BandwidthProgress::Downloading { current_mbps, progress_pct } => {
                render_bandwidth_panel(frame, main_layout[1], "Download", *current_mbps, 0.0, *progress_pct, false);
            }
            BandwidthProgress::Uploading { download_result_mbps, current_mbps, progress_pct } => {
                render_bandwidth_panel(frame, main_layout[1], "Upload", *download_result_mbps, *current_mbps, *progress_pct, false);
            }
            BandwidthProgress::Finished { download_mbps, upload_mbps } => {
                render_bandwidth_panel(
                    frame,
                    main_layout[1],
                    "Finished",
                    *download_mbps,
                    *upload_mbps,
                    100.0,
                    true,
                );
            }
        },
    }
}

fn render_bandwidth_panel(
    frame: &mut Frame,
    area: Rect,
    phase: &'static str,
    down_val: f64,
    up_val: f64,
    progress: f64,
    is_finished: bool,
) {
    let right_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let top_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(right_layout[0]);

    // Download block
    let down_str = format!("{:.1}", down_val);
    let down_color = if phase == "Download" {
        Color::LightCyan
    } else {
        Color::DarkGray
    };

    let down_block = Block::default()
        .title(" Download (Mbps) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(down_color));
    let down_inner = down_block.inner(top_split[0]);
    frame.render_widget(down_block, top_split[0]);

    let down_text = BigText::builder()
        .pixel_size(PixelSize::Full)
        .style(Style::default().fg(down_color))
        .lines(vec![down_str.into()])
        .build();
    frame.render_widget(down_text, down_inner);

    // Upload block
    let up_str = format!("{:.1}", up_val);
    let up_color = if phase == "Upload" {
        Color::LightMagenta
    } else {
        Color::DarkGray
    };

    let up_block = Block::default()
        .title(" Upload (Mbps) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(up_color));
    let up_inner = up_block.inner(top_split[1]);
    frame.render_widget(up_block, top_split[1]);

    let up_text = BigText::builder()
        .pixel_size(PixelSize::Full)
        .style(Style::default().fg(up_color))
        .lines(vec![up_str.into()])
        .build();
    frame.render_widget(up_text, up_inner);

    // Bottom block
    let bottom_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3), // Gauge
            Constraint::Length(1),
        ])
        .flex(ratatui::layout::Flex::Center)
        .split(right_layout[1]);

    if is_finished {
        let msg = Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
            "Test Complete. Press [Enter] to return to Ping View.",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )])]))
        .alignment(Alignment::Center);
        frame.render_widget(msg, bottom_layout[1]);
    } else {
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} Progress ", phase)),
            )
            .gauge_style(Style::default().fg(Color::LightCyan).bg(Color::DarkGray))
            .percent(progress.min(100.0).max(0.0) as u16);
        frame.render_widget(gauge, bottom_layout[1]);
    }
}
