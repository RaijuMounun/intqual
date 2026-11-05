use super::{NetworkProbe, TelemetryEvent};
use crate::models::{ProbeError, TracerouteHop};
use crate::network::icmp::packet::IcmpResponse;
use crate::network::icmp::{IcmpProvider, TracerouteIcmpProvider};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What one ICMP answer means for the trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HopVerdict {
    /// The destination itself answered; becomes `TracerouteHop::is_destination`.
    pub reached: bool,
    /// No point probing further TTLs.
    pub stop: bool,
    pub icmp_error: Option<String>,
}

/// Classifies one ICMP answer from `responder` to a probe aimed at `target`.
///
/// Time Exceeded in transit (type 11, code 0) is how every intermediate router answers a
/// traceroute, so it is not an error: reporting it would make a healthy 18-hop path look like
/// 18 failures to a script or an AI reading the JSON. Real problems stay visible: Destination
/// Unreachable, fragment-reassembly Time Exceeded (code 1) and unknown types.
///
/// Destination Unreachable always ends the trace, but it only counts as reaching the
/// destination when the target sent it (protocol/port unreachable from the host itself). From
/// a router it means the path is broken there, so the trace must not report `complete`.
pub(crate) fn classify_response(
    response: &IcmpResponse,
    responder: IpAddr,
    target: IpAddr,
) -> HopVerdict {
    let verdict = |reached, stop, icmp_error| HopVerdict {
        reached,
        stop,
        icmp_error,
    };
    // ICMPv6 numbers the same messages differently (RFC 4443): unreachable 1, time exceeded 3.
    let (unreachable, exceeded) = if target.is_ipv6() { (1, 3) } else { (3, 11) };
    match response {
        IcmpResponse::EchoReply(_) => verdict(true, true, None),
        IcmpResponse::TimeExceeded(t) if t.code == 0 => verdict(false, false, None),
        IcmpResponse::TimeExceeded(t) => verdict(
            false,
            false,
            Some(format!("Type: {exceeded}, Code: {}", t.code)),
        ),
        IcmpResponse::DestinationUnreachable(d) => verdict(
            responder == target,
            true,
            Some(format!("Type: {unreachable}, Code: {}", d.code)),
        ),
        IcmpResponse::Unknown { type_, code } => verdict(
            false,
            false,
            Some(format!("Type: {}, Code: {}", type_, code)),
        ),
    }
}

pub struct TracerouteProbe {
    pub resolved_addr: SocketAddr,
    pub timeout: Duration,
    pub max_hops: u8,
    pub icmp_identifier: u16,
}

impl TracerouteProbe {
    pub fn new(
        resolved_addr: SocketAddr,
        timeout: Duration,
        max_hops: u8,
        icmp_identifier: u16,
    ) -> Self {
        Self {
            resolved_addr,
            timeout,
            max_hops,
            icmp_identifier,
        }
    }
}

impl NetworkProbe for TracerouteProbe {
    async fn run(
        &mut self,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel_token: CancellationToken,
    ) -> Result<(), ProbeError> {
        for ttl in 1..=self.max_hops {
            if cancel_token.is_cancelled() {
                break;
            }

            let mut rtt_sum = 0.0;
            let mut rtt_count = 0;
            let mut last_responder = None;
            let mut is_dest_reached = false;
            let mut stop = false;
            let mut fatal_error = None;
            let mut last_icmp_err = None;

            for probe_idx in 0..3 {
                let seq = (ttl as u16) * 3 + probe_idx;
                let target_addr = self.resolved_addr;
                let timeout_duration = self.timeout;
                let identifier = self.icmp_identifier;
                let ttl_u32 = ttl as u32;

                let hop_result = {
                    tracing::debug!(
                        "Sending probe TTL={}, Seq={}, ID={}",
                        ttl_u32,
                        seq,
                        identifier
                    );
                    let provider = TracerouteIcmpProvider::new(identifier);
                    provider
                        .send_with_ttl(&target_addr, seq, ttl_u32, timeout_duration)
                        .await
                };

                match hop_result {
                    Ok(result) => {
                        tracing::debug!("Received response for TTL={}: {:?}", ttl, result.response);
                        let verdict = classify_response(
                            &result.response,
                            result.responder_ip,
                            self.resolved_addr.ip(),
                        );
                        stop |= verdict.stop;
                        let (is_dest, icmp_error_msg) = (verdict.reached, verdict.icmp_error);

                        rtt_sum += result.rtt_ms;
                        rtt_count += 1;
                        last_responder = Some(result.responder_ip.to_string());

                        if is_dest {
                            is_dest_reached = true;
                        }

                        if icmp_error_msg.is_some() {
                            last_icmp_err = icmp_error_msg;
                        }
                    }
                    Err(ProbeError::IcmpTimeout) => {
                        tracing::warn!(
                            "Timeout/Error for TTL={}, Probe={}: {}",
                            ttl,
                            probe_idx,
                            ProbeError::IcmpTimeout
                        );
                    }
                    Err(e) => {
                        tracing::error!("Fatal Error for TTL={}: {}", ttl, e);
                        fatal_error = Some(e);
                        break;
                    }
                }

                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            if let Some(err) = fatal_error {
                tracing::error!("Traceroute probe failing due to fatal error: {}", err);
                return Err(err);
            }

            let hop = if rtt_count > 0 {
                TracerouteHop {
                    hop_number: ttl,
                    ip_address: last_responder,
                    avg_rtt_ms: Some(rtt_sum / rtt_count as f64),
                    is_destination: is_dest_reached,
                    icmp_error: last_icmp_err,
                }
            } else {
                TracerouteHop {
                    hop_number: ttl,
                    ip_address: None,
                    avg_rtt_ms: None,
                    is_destination: false,
                    icmp_error: None,
                }
            };

            if let Err(e) = tx.try_send(TelemetryEvent::TracerouteHop(hop.clone())) {
                match e {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                        // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                        tracing::warn!(target: "probe_telemetry", "Telemetry channel full, shedding load / dropping probe frame");
                    }
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        return Ok(());
                    }
                }
            }

            // DNS resolution is handled asynchronously by the parent process (TUI)
            // in accordance with the Principle of Least Privilege.

            if is_dest_reached || stop {
                break;
            }
        }

        if let Err(e) = tx.try_send(TelemetryEvent::TracerouteComplete) {
            match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    // UI overloaded, intentionally dropping telemetry frame to prevent memory exhaustion
                    tracing::warn!(target: "probe_telemetry", "Telemetry channel full, shedding load / dropping probe frame");
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::icmp::packet::{IcmpDestUnreachable, IcmpEchoReply, IcmpTimeExceeded};
    use std::net::{IpAddr, Ipv4Addr};

    const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    const ROUTER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 254));

    fn verdict(reached: bool, stop: bool, icmp_error: Option<&str>) -> HopVerdict {
        HopVerdict {
            reached,
            stop,
            icmp_error: icmp_error.map(str::to_string),
        }
    }

    fn time_exceeded(code: u8) -> IcmpResponse {
        IcmpResponse::TimeExceeded(IcmpTimeExceeded {
            code,
            original_identifier: 1,
            original_sequence: 2,
        })
    }

    fn unreachable(code: u8) -> IcmpResponse {
        IcmpResponse::DestinationUnreachable(IcmpDestUnreachable {
            code,
            original_identifier: 1,
            original_sequence: 2,
        })
    }

    #[test]
    fn classify_time_exceeded_in_transit_is_a_normal_hop() {
        assert_eq!(
            classify_response(&time_exceeded(0), ROUTER, TARGET),
            verdict(false, false, None)
        );
    }

    #[test]
    fn classify_fragment_reassembly_time_exceeded_stays_an_error() {
        assert_eq!(
            classify_response(&time_exceeded(1), ROUTER, TARGET),
            verdict(false, false, Some("Type: 11, Code: 1"))
        );
    }

    #[test]
    fn classify_echo_reply_is_the_destination_without_error() {
        let reply = IcmpResponse::EchoReply(IcmpEchoReply {
            type_: 0,
            code: 0,
            identifier: 1,
            sequence_number: 2,
        });
        assert_eq!(
            classify_response(&reply, TARGET, TARGET),
            verdict(true, true, None)
        );
    }

    /// A router saying "host unreachable" ends the trace, but the destination was not
    /// reached: `complete` must stay false.
    #[test]
    fn classify_destination_unreachable_from_a_router_stops_without_reaching() {
        assert_eq!(
            classify_response(&unreachable(1), ROUTER, TARGET),
            verdict(false, true, Some("Type: 3, Code: 1"))
        );
    }

    /// The target itself answering "unreachable" (protocol or port) means the packet got there.
    #[test]
    fn classify_destination_unreachable_from_the_target_counts_as_reached() {
        assert_eq!(
            classify_response(&unreachable(3), TARGET, TARGET),
            verdict(true, true, Some("Type: 3, Code: 3"))
        );
    }

    /// ICMPv6 numbers unreachable as type 1 and time exceeded as type 3 (RFC 4443).
    #[test]
    fn classify_uses_icmpv6_type_numbers_for_an_ipv6_target() {
        let target: IpAddr = "2001:db8::1".parse().unwrap();
        let router: IpAddr = "2001:db8::fe".parse().unwrap();
        assert_eq!(
            classify_response(&unreachable(4), target, target),
            verdict(true, true, Some("Type: 1, Code: 4"))
        );
        assert_eq!(
            classify_response(&time_exceeded(1), router, target),
            verdict(false, false, Some("Type: 3, Code: 1"))
        );
        assert_eq!(
            classify_response(&time_exceeded(0), router, target),
            verdict(false, false, None)
        );
    }

    #[test]
    fn classify_unknown_type_keeps_the_error() {
        let unknown = IcmpResponse::Unknown { type_: 5, code: 1 };
        assert_eq!(
            classify_response(&unknown, ROUTER, TARGET),
            verdict(false, false, Some("Type: 5, Code: 1"))
        );
    }
}
