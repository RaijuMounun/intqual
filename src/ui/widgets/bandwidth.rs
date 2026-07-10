use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, Gauge, Paragraph},
};
use tui_big_text::{BigText, PixelSize};
use crate::models::{BandwidthProgress, ProbeError};
use crate::ui::{AppMode, AppState};
use super::AppWidget;

pub struct BandwidthWidget;

impl AppWidget for BandwidthWidget {
    fn render(&self, frame: &mut Frame, area: Rect, app: &AppState) {
        let main_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(area);

        let is_testing = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Downloading {..} | BandwidthProgress::Uploading {..}));
        let is_finished = matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Finished {..}));

        let mut stats_lines = Vec::new();

        if is_testing {
            stats_lines.push(Line::from(vec![Span::styled(
                "[TESTING BANDWIDTH...]",
                Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
            )]));
            stats_lines.push(Line::from(""));
        } else if is_finished {
            stats_lines.push(Line::from(vec![Span::styled(
                "[TEST FINISHED]",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )]));
            stats_lines.push(Line::from(""));
        } else if matches!(app.mode, AppMode::BandwidthTesting(BandwidthProgress::Failed(_))) {
            stats_lines.push(Line::from(vec![Span::styled(
                "[TEST FAILED]",
                Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
            )]));
            stats_lines.push(Line::from(""));
        }

        let loss_pct = app.icmp_stats.loss_pct;
        let jitter = app.icmp_stats.avg_jitter;

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

            let gray = Style::default().fg(Color::DarkGray);
            let mut icmp_color = gray;
            let tcp_color = gray;
            let jitter_style = gray;
            let loss_style = gray;

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
            stats_lines.push(Line::from(vec![Span::styled(format!(" Down: {:.1} Mbps", down), Style::default().fg(Color::LightCyan))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" Up:   {:.1} Mbps", up), Style::default().fg(Color::LightMagenta))]));
        }

        if let Some(ref err) = app.last_error {
            stats_lines.push(Line::from(""));
            stats_lines.push(Line::from(vec![Span::styled(" Bandwidth Error:", Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))]));
            stats_lines.push(Line::from(vec![Span::styled(format!(" {}", err), Style::default().fg(Color::LightRed))]));
        }

        let stats_block = Paragraph::new(Text::from(stats_lines)).block(
            Block::default()
                .title(" Live Metrics ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        frame.render_widget(stats_block, main_layout[0]);

        if let AppMode::BandwidthTesting(ref progress) = app.mode {
            match progress {
                BandwidthProgress::Downloading { current_mbps, progress_pct } => {
                    self.render_bandwidth_panel(frame, main_layout[1], "Download", *current_mbps, 0.0, *progress_pct, false);
                }
                BandwidthProgress::Uploading { download_result_mbps, current_mbps, progress_pct } => {
                    self.render_bandwidth_panel(frame, main_layout[1], "Upload", *download_result_mbps, *current_mbps, *progress_pct, false);
                }
                BandwidthProgress::Finished { download_mbps, upload_mbps } => {
                    self.render_bandwidth_panel(
                        frame,
                        main_layout[1],
                        "Finished",
                        *download_mbps,
                        *upload_mbps,
                        100.0,
                        true,
                    );
                }
                BandwidthProgress::Failed(msg) => {
                    let layout = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Percentage(30), Constraint::Length(5), Constraint::Percentage(30)])
                        .split(main_layout[1]);
                        
                    let error_text = format!("\n{}\n", msg);
                    let err_p = Paragraph::new(error_text)
                        .block(Block::default().title(" Bandwidth Test Failed ").borders(Borders::ALL).border_style(Style::default().fg(Color::Red)))
                        .style(Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))
                        .alignment(Alignment::Center)
                        .wrap(ratatui::widgets::Wrap { trim: true });
                        
                    frame.render_widget(err_p, layout[1]);
                    
                    let help_msg = Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
                        "Press [Enter] to return to Ping View.",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )])]))
                    .alignment(Alignment::Center);
                    
                    let help_layout = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(2), Constraint::Length(1)])
                        .split(layout[2]);
                        
                    frame.render_widget(help_msg, help_layout[1]);
                }
            }
        }
    }
}

impl BandwidthWidget {
    #[allow(clippy::too_many_arguments)]
    fn render_bandwidth_panel(
        &self,
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
        let down_color = if phase == "Download" { Color::LightCyan } else { Color::DarkGray };

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
        let up_color = if phase == "Upload" { Color::LightMagenta } else { Color::DarkGray };

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
            .constraints([Constraint::Length(1), Constraint::Length(3), Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .split(right_layout[1]);

        if is_finished {
            let msg = Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
                "Test Complete. Press [Enter] to return to Ping View.",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )])]))
            .alignment(Alignment::Center);
            frame.render_widget(msg, bottom_layout[1]);
        } else {
            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title(format!(" {} Progress ", phase)))
                .gauge_style(Style::default().fg(Color::LightCyan).bg(Color::DarkGray))
                .percent(progress.clamp(0.0, 100.0) as u16);
            frame.render_widget(gauge, bottom_layout[1]);
        }
    }
}
