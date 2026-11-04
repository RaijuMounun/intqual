use crate::ui::AppState;
use intqual_core::models::ProbeError;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    symbols,
    widgets::{
        Axis, Block, Borders, Chart, Dataset, GraphType, LegendPosition, Paragraph, Sparkline,
    },
};

pub struct LatencyDashboardWidget;

/// Splits `(sequence, ms)` points into runs of consecutive sequence numbers.
///
/// Lost samples are not stored, so a jump in sequence numbers marks a loss. A single line
/// would bridge that jump and hide the outage; one dataset per run leaves a visible gap.
fn runs(points: &[(f64, f64)]) -> impl Iterator<Item = &[(f64, f64)]> {
    points.chunk_by(|a, b| b.0 - a.0 <= 1.0)
}

/// Adds one line dataset per run (see [`runs`]). Only the first run carries `name`, so
/// the legend lists each series once.
fn push_segments<'a>(
    datasets: &mut Vec<Dataset<'a>>,
    points: &'a [(f64, f64)],
    name: &'static str,
    color: Color,
) {
    for (i, run) in runs(points).enumerate() {
        let mut dataset = Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(color))
            .data(run);
        if i == 0 {
            dataset = dataset.name(name);
        }
        datasets.push(dataset);
    }
}

impl LatencyDashboardWidget {
    pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
        let main_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(area);

        let start_seq = app.latest_sequence.saturating_sub(100_u64); // HISTORY_SIZE is 100
        let loss_pct = app.icmp_stats.loss_pct;
        let jitter = app.icmp_stats.avg_jitter;

        let mut stats_lines = Vec::new();

        if let Some(ref metric) = app.latest_metric {
            let mut perm_denied = false;
            let (icmp_str, icmp_color_override) = match &metric.icmp_ping {
                None => ("Waiting...".to_string(), None),
                Some(Ok(ms)) => (format!("{:.1} ms", ms), None),
                Some(Err(ProbeError::IcmpTimeout)) => (
                    "Timeout".to_string(),
                    Some(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ),
                Some(Err(ProbeError::PermissionDenied)) => {
                    perm_denied = true;
                    (
                        "Perm Denied".to_string(),
                        Some(
                            Style::default()
                                .fg(Color::LightRed)
                                .add_modifier(Modifier::BOLD),
                        ),
                    )
                }
                Some(Err(e)) => (
                    e.to_string(),
                    Some(
                        Style::default()
                            .fg(Color::LightRed)
                            .add_modifier(Modifier::BOLD),
                    ),
                ),
            };
            let tcp_str = match &metric.tcp_ping {
                None => "Waiting...".to_string(),
                Some(Ok(ms)) => format!("{:.1} ms", ms),
                Some(Err(e)) => e.to_string(),
            };

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

            let mut icmp_color = Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD);
            let tcp_color = Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD);
            let jitter_style = j_style;
            let loss_style = l_style;

            if let Some(c) = icmp_color_override {
                icmp_color = c;
            }

            if perm_denied {
                stats_lines.push(Line::from(vec![Span::styled(
                    " NO RAW SOCKET PERMISSIONS ",
                    Style::default()
                        .bg(Color::Red)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
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
            // Errors that are not loss (refused port, no permission) stay out of loss %,
            // so they get their own line rather than vanishing.
            let errors = app.icmp_stats.error_count + app.tcp_stats.error_count;
            if errors > 0 {
                stats_lines.push(Line::from(vec![Span::styled(
                    " Errors (not loss):",
                    Style::default().fg(Color::DarkGray),
                )]));
                stats_lines.push(Line::from(vec![Span::styled(
                    format!(" {} probes", errors),
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(
                format!(" Seq ID: {}", metric.sequence_number),
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            stats_lines.push(Line::from("  Waiting for data..."));
        }

        if let Some((down, up)) = app.last_speed_test {
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(
                " Last Speed Test:",
                Style::default().fg(Color::DarkGray),
            )]));
            stats_lines.push(Line::from(vec![
                Span::styled(" Down: ", Style::default().fg(Color::LightCyan)),
                Span::styled(
                    format!("{:.1} Mbps", down),
                    Style::default().fg(Color::LightCyan),
                ),
            ]));
            stats_lines.push(Line::from(vec![
                Span::styled(" Up:   ", Style::default().fg(Color::LightMagenta)),
                Span::styled(
                    format!("{:.1} Mbps", up),
                    Style::default().fg(Color::LightMagenta),
                ),
            ]));
        }

        let stats_block = Paragraph::new(Text::from(stats_lines)).block(
            Block::default()
                .title(" Live Metrics ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        frame.render_widget(stats_block, main_layout[0]);

        let right_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(main_layout[1]);

        let mut datasets = Vec::new();
        push_segments(
            &mut datasets,
            app.tcp_data.as_slices().0,
            "TCP (App)",
            Color::DarkGray,
        );
        push_segments(
            &mut datasets,
            app.icmp_data.as_slices().0,
            "ICMP (Ping)",
            Color::LightCyan,
        );

        let x_bounds = [start_seq as f64, app.latest_sequence as f64];
        let max_val = app.chart_max_ping;
        let y_bounds = [0.0, max_val * 1.1];

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
                        Span::raw(format!("{:.0}", max_val / 2.0)),
                        Span::raw(format!("{:.0}", max_val)),
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
            .data(app.jitter_history.as_slices().0)
            .style(Style::default().fg(jitter_color));

        frame.render_widget(jitter_sparkline, right_layout[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use intqual_core::models::PingMetrics;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_to_string(app: &AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal
            .draw(|frame| LatencyDashboardWidget::render(frame, frame.area(), app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn seqs(run: &[(f64, f64)]) -> Vec<f64> {
        run.iter().map(|p| p.0).collect()
    }

    #[test]
    fn runs_split_at_sequence_gaps() {
        let points = [(1.0, 10.0), (2.0, 11.0), (4.0, 12.0), (5.0, 13.0)];
        let split: Vec<_> = runs(&points).map(seqs).collect();
        assert_eq!(split, vec![vec![1.0, 2.0], vec![4.0, 5.0]]);
    }

    #[test]
    fn consecutive_points_form_one_run() {
        let points = [(1.0, 10.0), (2.0, 11.0), (3.0, 12.0)];
        assert_eq!(runs(&points).count(), 1);
        assert_eq!(runs(&[]).count(), 0);
    }

    #[test]
    fn missing_protocol_result_shows_waiting_not_timeout() {
        let mut app = AppState::new();
        app.latest_metric = Some(PingMetrics {
            sequence_number: 1,
            target_ip: "192.0.2.1".into(),
            icmp_ping: Some(Ok(12.0)),
            tcp_ping: None,
            timestamp: 0,
        });
        let screen = render_to_string(&app);
        assert!(screen.contains("12.0 ms"));
        assert!(screen.contains("Waiting..."));
        assert!(!screen.contains("Timeout"));
    }

    #[test]
    fn non_loss_errors_get_their_own_line() {
        let mut app = AppState::new();
        app.latest_metric = Some(PingMetrics {
            sequence_number: 1,
            target_ip: "192.0.2.1".into(),
            icmp_ping: None,
            tcp_ping: None,
            timestamp: 0,
        });
        assert!(!render_to_string(&app).contains("Errors (not loss)"));

        app.tcp_stats.error_count = 3;
        let screen = render_to_string(&app);
        assert!(screen.contains("Errors (not loss)"));
        assert!(screen.contains("3 probes"));
    }
}
