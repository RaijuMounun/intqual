use std::net::SocketAddr;
use std::time::Duration;

pub trait IcmpProvider {
    fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, crate::models::ProbeError>;
}
