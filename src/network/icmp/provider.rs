use std::net::{SocketAddr, IpAddr};
use std::time::Duration;
use super::packet::IcmpResponse;

#[derive(Debug)]
pub struct TracerouteHopResult {
    pub rtt_ms: f64,
    pub responder_ip: IpAddr,
    pub response: IcmpResponse,
}

pub trait IcmpProvider {
    fn ping(&self, target: &SocketAddr, seq: u16, timeout: Duration) -> Result<f64, crate::models::ProbeError>;
    fn send_with_ttl(&self, target: &SocketAddr, seq: u16, ttl: u32, timeout: Duration) -> Result<TracerouteHopResult, crate::models::ProbeError>;
}
