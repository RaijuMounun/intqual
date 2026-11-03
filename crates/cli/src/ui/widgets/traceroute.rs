use crate::ui::AppState;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
};

#[derive(Default)]
pub struct TracerouteWidget;

impl TracerouteWidget {
    pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
        let mut rows = Vec::new();

        for hop in &app.traceroute_hops {
            let hop_num = hop.hop_number.to_string();

            let (ip_str, hostname_str, hostname_style, rtt_str, style) =
                if let Some(ip) = &hop.ip_address {
                    let rtt = if let Some(avg_rtt_ms) = hop.avg_rtt_ms {
                        format!("{:.1} ms", avg_rtt_ms)
                    } else {
                        "*".to_string()
                    };

                    let style = if hop.is_destination {
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    let (h_str, h_style) = match app.dns_status.get(ip) {
                        Some(crate::ui::DnsStatus::Resolving) => (
                            "Resolving...".to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Some(crate::ui::DnsStatus::Resolved(name)) => (name.clone(), style),
                        Some(crate::ui::DnsStatus::Failed) => {
                            ("Unknown".to_string(), Style::default().fg(Color::DarkGray))
                        }
                        None => ("Unknown".to_string(), Style::default().fg(Color::DarkGray)),
                    };

                    (ip.as_str(), h_str, h_style, rtt, style)
                } else {
                    (
                        "*",
                        "*".to_string(),
                        Style::default().fg(Color::DarkGray),
                        "*".to_string(),
                        Style::default().fg(Color::DarkGray),
                    )
                };

            rows.push(Row::new(vec![
                Cell::from(hop_num).style(style),
                Cell::from(ip_str).style(style),
                Cell::from(hostname_str).style(hostname_style),
                Cell::from(rtt_str).style(style),
            ]));
        }

        let header = Row::new(vec!["#", "IP Address", "Hostname", "RTT (ms)"])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let title = if app.last_error.is_some() {
            " Traceroute (Failed) "
        } else if app.traceroute_complete {
            " Traceroute (Complete) "
        } else {
            " Traceroute (Running...) "
        };

        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let table_area = if let Some(err) = &app.last_error {
            let [error_area, rest] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
            let error = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::LightRed))
                .wrap(Wrap { trim: true });
            frame.render_widget(error, error_area);
            rest
        } else {
            inner
        };

        let table = Table::new(
            rows,
            [
                Constraint::Length(5),
                Constraint::Length(20),
                Constraint::Percentage(40),
                Constraint::Length(15),
            ],
        )
        .header(header);

        frame.render_widget(table, table_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_to_string(app: &AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| TracerouteWidget::render(frame, frame.area(), app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn shows_last_error_on_the_traceroute_screen() {
        let mut app = AppState::new();
        app.last_error = Some("Traceroute Error: ICMP Timeout".to_string());
        let screen = render_to_string(&app);
        assert!(screen.contains("Traceroute (Failed)"));
        assert!(screen.contains("Traceroute Error: ICMP Timeout"));
    }

    #[test]
    fn running_screen_has_no_error_line() {
        let screen = render_to_string(&AppState::new());
        assert!(screen.contains("Traceroute (Running...)"));
        assert!(!screen.contains("Error"));
    }
}
