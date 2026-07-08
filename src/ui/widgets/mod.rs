use ratatui::prelude::{Frame, Rect};
use crate::ui::AppState;

pub trait AppWidget: Send + Sync {
    fn render(&self, frame: &mut Frame, area: Rect, app: &AppState);
}

pub mod latency;
pub mod bandwidth;
pub mod traceroute;
