use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, LegendPosition, Sparkline},
    symbols,
};
use std::io::{stdout, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use crate::models::NetworkMetrics;
use crate::engine::core_engine::EngineCommand;

/// Defines the maximum number of data points retained in memory for rendering.
/// 100 perfectly balances the memory footprint with optimal visual data density 
/// for standard terminal widths, ensuring the sliding window remains legible.
const HISTORY_SIZE: usize = 100;

pub struct AppState {
    pub history: Vec<Option<NetworkMetrics>>,
    pub latest_sequence: u64,
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
        }
    }

    /// Ingests a new metric into the ring buffer.
    pub fn push_metric(&mut self, metric: NetworkMetrics) {
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
                        Err(_) => {
                            loss_count += 1;
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
}

/// The main synchronous event loop for the Terminal User Interface.
pub fn run_app(mut rx: mpsc::Receiver<NetworkMetrics>, _cmd_tx: mpsc::Sender<EngineCommand>) -> Result<()> {
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

        while let Ok(metric) = rx.try_recv() {
            app.push_metric(metric);
            state_changed = true;
        }

        if state_changed {
            terminal.draw(|frame| draw_ui(frame, &app))?;
        }

        // Throttle the event poller to 50ms (20 FPS) to drastically reduce idle CPU footprint.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q') {
                        break;
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

    // 1. MACRO LAYOUT (F-Pattern)
    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(area);

    
    let right_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(main_layout[1]);

    // 2. DATA EXTRACTION
    let mut icmp_data: Vec<(f64, f64)> = Vec::new();
    let mut tcp_data: Vec<(f64, f64)> = Vec::new();
    let mut jitter_history: Vec<u64> = Vec::new();
    let mut max_ping: f64 = 50.0; 

    let start_seq = app.latest_sequence.saturating_sub(HISTORY_SIZE as u64);
    let mut last_icmp_ping: Option<f64> = None;
    
    for seq in start_seq..=app.latest_sequence {
        let idx = (seq % HISTORY_SIZE as u64) as usize;
        if let Some(ref metric) = app.history[idx] {
            if metric.sequence_number == seq {
                if let Ok(ping) = metric.icmp_ping {
                    icmp_data.push((seq as f64, ping));
                    if ping > max_ping { max_ping = ping; }

                    if let Some(last) = last_icmp_ping {
                        let j = (ping - last).abs();
                        jitter_history.push(j.round() as u64);
                    } else {
                        jitter_history.push(0);
                    }
                    last_icmp_ping = Some(ping);
                } else {
                    icmp_data.push((seq as f64, 0.0));
                    jitter_history.push(0);
                }

                if let Ok(ping) = metric.tcp_ping {
                    tcp_data.push((seq as f64, ping));
                    if ping > max_ping { max_ping = ping; }
                }
            }
        } else {
            jitter_history.push(0);
        }
    }

    // 3. LEFT COLUMN (Actionable Metrics & Alarms)
    let (loss_pct, jitter) = app.calculate_stats();
    let latest_idx = (app.latest_sequence % HISTORY_SIZE as u64) as usize;
    
    let mut stats_lines = Vec::new();

    if app.latest_sequence == 0 {
        stats_lines.push(Line::from("  Waiting for data..."));
    } else if let Some(ref metric) = app.history[latest_idx] {
        let icmp_str = match &metric.icmp_ping {
            Ok(ms) => format!("{:.1} ms", ms),
            Err(e) => e.clone(),
        };
        let tcp_str = match &metric.tcp_ping {
            Ok(ms) => format!("{:.1} ms", ms),
            Err(e) => e.clone(),
        };
        
        stats_lines.push(Line::from(vec![Span::styled(" Target:", Style::default().fg(Color::DarkGray))]));
        stats_lines.push(Line::from(vec![Span::styled(format!(" {}", metric.target_ip), Style::default().add_modifier(Modifier::BOLD))]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(" ICMP Ping (Network):", Style::default().fg(Color::DarkGray))]));
        stats_lines.push(Line::from(vec![Span::styled(format!(" {}", icmp_str), Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD))]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(" TCP Ping (App Layer):", Style::default().fg(Color::DarkGray))]));
        stats_lines.push(Line::from(vec![Span::styled(format!(" {}", tcp_str), Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD))]));
        stats_lines.push(Line::from(""));

        // DYNAMIC ALARM: Moderate instability warning threshold (20ms)
        let jitter_style = if jitter > 20.0 {
            Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        };
        stats_lines.push(Line::from(vec![Span::styled(" Jitter (Stability):", Style::default().fg(Color::DarkGray))]));
        stats_lines.push(Line::from(vec![Span::styled(format!(" {:.1} ms", jitter), jitter_style)]));
        stats_lines.push(Line::from(""));

        // DYNAMIC ALARM: Catastrophic failure detection
        // Inverting colors (Red BG/Black Text) creates an immediate, unignorable visual pop.
        let loss_style = if loss_pct > 0.0 {
            Style::default().fg(Color::Black).bg(Color::LightRed).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        };
        stats_lines.push(Line::from(vec![Span::styled(" Pkt Loss (Survival):", Style::default().fg(Color::DarkGray))]));
        stats_lines.push(Line::from(vec![Span::styled(format!(" {:.1}%", loss_pct), loss_style)]));
        stats_lines.push(Line::from(""));

        stats_lines.push(Line::from(vec![Span::styled(format!(" Seq ID: {}", metric.sequence_number), Style::default().fg(Color::DarkGray))]));
    }

    let stats_block = Paragraph::new(Text::from(stats_lines))
        .block(Block::default().title(" Live Metrics ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));

    frame.render_widget(stats_block, main_layout[0]);

    // 4. RIGHT TOP PANEL (Latency Lines)
    let datasets = vec![
        Dataset::default()
            .name("TCP (App)")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray))
            .data(&tcp_data),
        Dataset::default()
            .name("ICMP (Ping)")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::LightCyan))
            .data(&icmp_data),
    ];

    let x_bounds = [start_seq as f64, app.latest_sequence as f64];
    let y_bounds = [0.0, max_ping * 1.1]; 

    let chart = Chart::new(datasets)
        .block(Block::default().title(" Latency History (ms) [Exit: Q] ").borders(Borders::ALL).border_style(Style::default().fg(Color::LightCyan)))
        .x_axis(Axis::default().style(Style::default().fg(Color::DarkGray)).bounds(x_bounds))
        .y_axis(
            Axis::default()
                .title("ms")
                .style(Style::default().fg(Color::DarkGray))
                .bounds(y_bounds)
                .labels(vec![
                    Span::raw("0"),
                    Span::raw(format!("{:.0}", max_ping / 2.0)),
                    Span::raw(format!("{:.0}", max_ping)),
                ]),
        )
        .legend_position(Some(LegendPosition::TopLeft));

    frame.render_widget(chart, right_layout[0]);

    // 5. RIGHT BOTTOM PANEL (Gestalt Jitter Sparkline)
    // WHY Gestalt: Differentiating the form factor (Lines for latency, Bars for jitter) 
    // prevents cognitive overload while keeping their X-axes perfectly synced vertically.
    let jitter_color = if jitter > 20.0 { Color::LightYellow } else { Color::Magenta };
    
    let jitter_sparkline = Sparkline::default()
        .block(Block::default().title(" Jitter Deviation Trend (Gestalt Bar Chart) ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
        .data(&jitter_history)
        .style(Style::default().fg(jitter_color));

    frame.render_widget(jitter_sparkline, right_layout[1]);
}