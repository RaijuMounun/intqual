use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, LegendPosition},
    symbols,
};
use std::io::{stdout, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use crate::models::NetworkMetrics;

const HISTORY_SIZE: usize = 100;

pub struct AppState {
    pub history: Vec<Option<NetworkMetrics>>,
    pub latest_sequence: u64,
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
        }
    }

    pub fn push_metric(&mut self, metric: NetworkMetrics) {
        if metric.sequence_number > self.latest_sequence {
            self.latest_sequence = metric.sequence_number;
        }
        let index = (metric.sequence_number % HISTORY_SIZE as u64) as usize;
        self.history[index] = Some(metric);
    }

    /// Calculates Packet Loss (%) and Jitter (ms) based on the current history buffer.
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
                            // Jitter calculation: absolute difference between consecutive successful pings
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

pub fn run_app(mut rx: mpsc::Receiver<NetworkMetrics>) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = AppState::new();

    // Uygulama ilk açıldığında boş ekranı 1 kere çiz
    terminal.draw(|frame| draw_ui(frame, &app))?;

    loop {
        let mut state_changed = false;

        // Kuyruktaki yeni verileri topla
        while let Ok(metric) = rx.try_recv() {
            app.push_metric(metric);
            state_changed = true; // Veri değişti, ekran "Kirli" (Dirty)
        }

        // OPTİMİZASYON: Sadece yeni veri geldiyse GPU'yu yor ve ekranı tekrar çiz!
        if state_changed {
            terminal.draw(|frame| draw_ui(frame, &app))?;
        }

        // UI Event Loop (Klavye ve Yeniden Boyutlandırma dinleyicisi)
        // CPU'yu dinlendirmek için 50ms bekle (20 FPS tepkime süresi fazlasıyla yeterli)
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q') {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    // Kullanıcı terminal penceresini büyütüp küçülttüğünde zorla yeniden çiz
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

fn draw_ui(frame: &mut Frame, app: &AppState) {
    let area = frame.area();

    // 1. LAYOUT ARCHITECTURE
    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(area);

    // 2. DATA PREPARATION
    let mut icmp_data: Vec<(f64, f64)> = Vec::new();
    let mut tcp_data: Vec<(f64, f64)> = Vec::new();
    let mut max_ping: f64 = 50.0; 

    let start_seq = app.latest_sequence.saturating_sub(HISTORY_SIZE as u64);
    
    for seq in start_seq..=app.latest_sequence {
        let idx = (seq % HISTORY_SIZE as u64) as usize;
        if let Some(ref metric) = app.history[idx] {
            if metric.sequence_number == seq {
                if let Ok(ping) = metric.icmp_ping {
                    icmp_data.push((seq as f64, ping));
                    if ping > max_ping { max_ping = ping; }
                }
                if let Ok(ping) = metric.tcp_ping {
                    tcp_data.push((seq as f64, ping));
                    if ping > max_ping { max_ping = ping; }
                }
            }
        }
    }

    // 3. LEFT COLUMN: LIVE METRICS
    let (loss_pct, jitter) = app.calculate_stats();
    let latest_idx = (app.latest_sequence % HISTORY_SIZE as u64) as usize;

    let stats_text = if app.latest_sequence == 0 {
        "\n  Waiting for data...".to_string()
    } else if let Some(ref metric) = app.history[latest_idx] {
        let icmp_str = match &metric.icmp_ping {
            Ok(ms) => format!("{:.1} ms", ms),
            Err(e) => e.clone(),
        };
        let tcp_str = match &metric.tcp_ping {
            Ok(ms) => format!("{:.1} ms", ms),
            Err(e) => e.clone(),
        };
        
        format!(
            "\n Target:\n {}\n\n ICMP Ping:\n {}\n\n TCP Ping:\n {}\n\n Jitter:\n {:.1} ms\n\n Pkt Loss:\n {:.1}%\n\n Seq ID: {}",
            metric.target_ip, icmp_str, tcp_str, jitter, loss_pct, metric.sequence_number
        )
    } else {
        "\n State Error".to_string()
    };

    let stats_block = Paragraph::new(stats_text)
        .block(Block::default().title(" Live Metrics ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    frame.render_widget(stats_block, main_layout[0]);

    // 4. RIGHT COLUMN: SLIDING WINDOW CHART
    let datasets = vec![
        // TCP rendered FIRST (Background)
        Dataset::default()
            .name("TCP (App)")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray))
            .data(&tcp_data),
        // ICMP rendered SECOND (Foreground)
        Dataset::default()
            .name("ICMP (Ping)")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::LightCyan)) // Made it brighter
            .data(&icmp_data),
    ];

    let x_bounds = [start_seq as f64, app.latest_sequence as f64];
    let y_bounds = [0.0, max_ping * 1.1]; 

    let chart = Chart::new(datasets)
        .block(Block::default().title(" Real-time Network Latency (ms) [Press 'Q' to exit] ").borders(Borders::ALL).border_style(Style::default().fg(Color::LightCyan)))
        .x_axis(
            Axis::default()
                .title("Time (Seq)")
                .style(Style::default().fg(Color::DarkGray))
                .bounds(x_bounds)
                .labels(vec![
                    Span::raw(format!("{}", start_seq)),
                    Span::raw(format!("{}", app.latest_sequence)),
                ]),
        )
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
        // Position legend in the top left to prevent blocking active right-side data
        .legend_position(Some(LegendPosition::TopLeft));

    frame.render_widget(chart, main_layout[1]);
}