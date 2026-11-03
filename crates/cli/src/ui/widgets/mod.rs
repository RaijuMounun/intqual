use crate::ui::AppState;
use ratatui::prelude::{Frame, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveWidget {
    Latency,
    Bandwidth,
    Traceroute,
    Error,
}

impl ActiveWidget {
    pub fn render(&self, frame: &mut Frame, area: Rect, app: &AppState) {
        match self {
            Self::Latency => latency::LatencyDashboardWidget::render(frame, area, app),
            Self::Bandwidth => bandwidth::BandwidthWidget::render(frame, area, app),
            Self::Traceroute => traceroute::TracerouteWidget::render(frame, area, app),
            Self::Error => error::ErrorWidget::render(frame, area, app),
        }
    }
}

pub mod bandwidth;
pub mod error;
pub mod latency;
pub mod traceroute;
