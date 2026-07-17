use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    symbols,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, LegendPosition, Paragraph, Sparkline},
};
use intqual_core::models::ProbeError;
use crate::ui::AppState;

pub struct LatencyDashboardWidget;

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

            let j_style = if jitter > 20.0 {
                Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
            };

            let l_style = if loss_pct > 0.0 {
                Style::default().fg(Color::Black).bg(Color::LightRed).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            };

            let mut icmp_color = Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD);
            let tcp_color = Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD);
            let jitter_style = j_style;
            let loss_style = l_style;

            if let Some(c) = icmp_color_override {
                icmp_color = c;
            }

            if perm_denied {
                stats_lines.push(Line::from(vec![Span::styled(" NO RAW SOCKET PERMISSIONS ", Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD))]));
                stats_lines.push(Line::from(""));
            }

            stats_lines.push(Line::from(vec![Span::styled(" Target:", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {}", metric.target_ip), Style::default().add_modifier(Modifier::BOLD))]));
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" ICMP Ping (Network):", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {}", icmp_str), icmp_color)]));
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" TCP Ping (App Layer):", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {}", tcp_str), tcp_color)]));
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" Jitter (Stability):", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {:.1} ms", jitter), jitter_style)]));
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" Pkt Loss (Survival):", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {:.1}%", loss_pct), loss_style)]));
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(format!(" Seq ID: {}", metric.sequence_number), Style::default().fg(Color::DarkGray))]));
        } else {
            stats_lines.push(Line::from("  Waiting for data..."));
        }

        if let Some((down, up)) = app.last_speed_test {
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" Last Speed Test:", Style::default().fg(Color::DarkGray))]));
            stats_lines.push(Line::from(vec![
                Span::styled(" Down: ", Style::default().fg(Color::LightCyan)),
                Span::styled(format!("{:.1} Mbps", down), Style::default().fg(Color::LightCyan)),
            ]));
            stats_lines.push(Line::from(vec![
                Span::styled(" Up:   ", Style::default().fg(Color::LightMagenta)),
                Span::styled(format!("{:.1} Mbps", up), Style::default().fg(Color::LightMagenta)),
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

        let datasets = vec![
            Dataset::default()
                .name("TCP (App)")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::DarkGray))
                .data(app.tcp_data.as_slices().0),
            Dataset::default()
                .name("ICMP (Ping)")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::LightCyan))
                .data(app.icmp_data.as_slices().0),
        ];

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
            .x_axis(Axis::default().style(Style::default().fg(Color::DarkGray)).bounds(x_bounds))
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
