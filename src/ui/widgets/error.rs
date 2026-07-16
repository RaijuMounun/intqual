use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use crate::ui::{AppState, AppMode};

#[derive(Default)]
pub struct ErrorWidget;

impl ErrorWidget {
    pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
        let error_msg = match &app.mode {
            AppMode::Error(msg) => msg.as_str(),
            _ => "Unknown Error",
        };

        let block = Block::default()
            .title(" Error ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));

        let text = format!("\n\nPermission Denied: Administrator privileges (sudo) are required.\n\nDetails:\n{}", error_msg);

        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Center)
            .block(block);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(50),
                Constraint::Percentage(25),
            ])
            .split(area);
            
        let inner_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(60),
                Constraint::Percentage(20),
            ])
            .split(layout[1]);

        frame.render_widget(paragraph, inner_layout[1]);
    }
}
