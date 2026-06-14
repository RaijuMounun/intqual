use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use std::io::{stdout, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use crate::models::NetworkMetrics;

/// The fixed capacity of our Ring Buffer.
/// Limits memory usage and determines how many data points are displayed.
const HISTORY_SIZE: usize = 100;

/// AppState holds the entire UI state (Model).
pub struct AppState {
    /// The Ring Buffer storing incoming metrics.
    pub history: Vec<Option<NetworkMetrics>>,
    /// Tracks the highest sequence number received to handle out-of-order data.
    pub latest_sequence: u64,
}

impl AppState {
    pub fn new() -> Self {
        // Initialize an empty ring buffer with fixed capacity
        let mut history = Vec::with_capacity(HISTORY_SIZE);
        for _ in 0..HISTORY_SIZE {
            history.push(None);
        }
        Self {
            history,
            latest_sequence: 0,
        }
    }

    /// Inserts a new metric into the Ring Buffer using its sequence number.
    /// This inherently solves the out-of-order packet delivery problem.
    pub fn push_metric(&mut self, metric: NetworkMetrics) {
        if metric.sequence_number > self.latest_sequence {
            self.latest_sequence = metric.sequence_number;
        }
        
        // Modulo operator ensures we wrap around and overwrite old data
        let index = (metric.sequence_number % HISTORY_SIZE as u64) as usize;
        self.history[index] = Some(metric);
    }
}

/// The main UI event loop (Controller & View).
/// Consumes metrics from the channel and renders them at 60 FPS.
pub fn run_app(mut rx: mpsc::Receiver<NetworkMetrics>) -> Result<()> {
    // 1. Setup Terminal (Enter Raw Mode to capture raw keypresses)
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = AppState::new();

    // 2. The Core Render Loop
    loop {
        // Drain all pending messages from the channel (Backpressure handling)
        // try_recv() doesn't block; it reads what's available and moves on.
        while let Ok(metric) = rx.try_recv() {
            app.push_metric(metric);
        }

        // Render the UI Frame
        terminal.draw(|frame| draw_ui(frame, &app))?;

        // 3. Event Polling (Non-blocking keyboard listener)
        // Wait up to 16ms (approx 60 FPS) for a key press
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break; // Exit the loop gracefully
                }
            }
        }
    }

    // 4. Teardown Terminal (Restore user's terminal state)
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    
    Ok(())
}

/// The specific View logic (Dumb UI).
/// Determines how the AppState is presented on the screen.
fn draw_ui(frame: &mut Frame, app: &AppState) {
    let size = frame.area();

    // Create a simple full-screen Bento Grid box
    let block = Block::default()
        .title(" intqual Network Engine [Press 'q' to exit] ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    // For the MVP, just show the latest raw data as text
    let display_text = if app.latest_sequence == 0 {
        "Waiting for data...".to_string()
    } else {
        // Fetch the most recent data point from the Ring Buffer
        let latest_idx = (app.latest_sequence % HISTORY_SIZE as u64) as usize;
        if let Some(ref metric) = app.history[latest_idx] {
            let tcp_status = match &metric.tcp_ping {
                Ok(ms) => format!("{:.2} ms", ms),
                Err(e) => format!("FAIL ({})", e),
            };
            
            format!(
                "Target: {}\nSequence: {}\nTCP Ping: {}",
                metric.target_ip, metric.sequence_number, tcp_status
            )
        } else {
            "Data missing (Out of order / Dropped)".to_string()
        }
    };

    let paragraph = Paragraph::new(display_text)
        .block(block)
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center);

    frame.render_widget(paragraph, size);
}