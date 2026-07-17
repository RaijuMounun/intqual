use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};
use crate::ui::AppState;

#[derive(Default)]
pub struct TracerouteWidget;

impl TracerouteWidget {
    pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
        let mut rows = Vec::new();

        for hop in &app.traceroute_hops {
            let hop_num = hop.hop_number.to_string();
            
            let (ip_str, hostname_str, hostname_style, rtt_str, style) = if let Some(ip) = &hop.ip_address {
                let rtt = if let Some(avg_rtt_ms) = hop.avg_rtt_ms {
                    format!("{:.1} ms", avg_rtt_ms)
                } else {
                    "*".to_string()
                };

                let style = if hop.is_destination {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let (h_str, h_style) = match app.dns_status.get(ip) {
                    Some(crate::ui::DnsStatus::Resolving) => ("Resolving...".to_string(), Style::default().fg(Color::DarkGray)),
                    Some(crate::ui::DnsStatus::Resolved(name)) => (name.clone(), style),
                    Some(crate::ui::DnsStatus::Failed) => ("Unknown".to_string(), Style::default().fg(Color::DarkGray)),
                    None => ("Unknown".to_string(), Style::default().fg(Color::DarkGray)),
                };

                (ip.as_str(), h_str, h_style, rtt, style)
            } else {
                ("*", "*".to_string(), Style::default().fg(Color::DarkGray), "*".to_string(), Style::default().fg(Color::DarkGray))
            };

            rows.push(Row::new(vec![
                Cell::from(hop_num).style(style),
                Cell::from(ip_str).style(style),
                Cell::from(hostname_str).style(hostname_style),
                Cell::from(rtt_str).style(style),
            ]));
        }

        let header = Row::new(vec!["#", "IP Address", "Hostname", "RTT (ms)"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .bottom_margin(1);

        let title = if app.traceroute_complete {
            " Traceroute (Complete) "
        } else {
            " Traceroute (Running...) "
        };

        let table = Table::new(rows, [
            Constraint::Length(5),
            Constraint::Length(20),
            Constraint::Percentage(40),
            Constraint::Length(15),
        ])
        .header(header)
        .block(Block::default().title(title).borders(Borders::ALL));

        frame.render_widget(table, area);
    }
}
