use super::packet::IcmpResponse;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

#[derive(Debug)]
pub struct TracerouteHopResult {
    pub rtt_ms: f64,
    pub responder_ip: IpAddr,
    pub response: IcmpResponse,
}
#[allow(async_fn_in_trait)]
pub trait IcmpProvider {
    async fn ping(
        &self,
        target: &SocketAddr,
        seq: u16,
        timeout: Duration,
    ) -> Result<f64, crate::models::ProbeError>;
    async fn send_with_ttl(
        &self,
        target: &SocketAddr,
        seq: u16,
        ttl: u32,
        timeout: Duration,
    ) -> Result<TracerouteHopResult, crate::models::ProbeError>;
}
