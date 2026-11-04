use super::{NetworkProbe, TelemetryEvent};
use crate::models::{ProbeError, TracerouteHop};
use crate::network::icmp::packet::IcmpResponse;
use crate::network::icmp::{IcmpProvider, TracerouteIcmpProvider};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Maps one ICMP answer to `(is_destination, icmp_error)`.
///
/// Time Exceeded in transit (type 11, code 0) is how every intermediate router answers a
/// traceroute, so it is not an error: reporting it would make a healthy 18-hop path look like
/// 18 failures to a script or an AI reading the JSON. Real problems stay visible: Destination
/// Unreachable, fragment-reassembly Time Exceeded (code 1) and unknown types.
pub(crate) fn classify_response(response: &IcmpResponse) -> (bool, Option<String>) {
    match response {
        IcmpResponse::EchoReply(_) => (true, None),
        IcmpResponse::TimeExceeded(t) if t.code == 0 => (false, None),
        IcmpResponse::TimeExceeded(t) => (false, Some(format!("Type: 11, Code: {}", t.code))),
        IcmpResponse::DestinationUnreachable(d) => {
            (true, Some(format!("Type: 3, Code: {}", d.code)))
        }
        IcmpResponse::Unknown { type_, code } => {
            (false, Some(format!("Type: {}, Code: {}", type_, code)))
        }
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
                        let (is_dest, icmp_error_msg) = classify_response(&result.response);

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

            if is_dest_reached {
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

    fn time_exceeded(code: u8) -> IcmpResponse {
        IcmpResponse::TimeExceeded(IcmpTimeExceeded {
            code,
            original_identifier: 1,
            original_sequence: 2,
        })
    }

    #[test]
    fn classify_time_exceeded_in_transit_is_a_normal_hop() {
        assert_eq!(classify_response(&time_exceeded(0)), (false, None));
    }

    #[test]
    fn classify_fragment_reassembly_time_exceeded_stays_an_error() {
        assert_eq!(
            classify_response(&time_exceeded(1)),
            (false, Some("Type: 11, Code: 1".to_string()))
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
        assert_eq!(classify_response(&reply), (true, None));
    }

    #[test]
    fn classify_destination_unreachable_keeps_the_error() {
        let unreachable = IcmpResponse::DestinationUnreachable(IcmpDestUnreachable {
            code: 3,
            original_identifier: 1,
            original_sequence: 2,
        });
        assert_eq!(
            classify_response(&unreachable),
            (true, Some("Type: 3, Code: 3".to_string()))
        );
    }

    #[test]
    fn classify_unknown_type_keeps_the_error() {
        let unknown = IcmpResponse::Unknown { type_: 5, code: 1 };
        assert_eq!(
            classify_response(&unknown),
            (false, Some("Type: 5, Code: 1".to_string()))
        );
    }
}
