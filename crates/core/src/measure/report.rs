//! The result of one `measure` call. The JSON form of these structs is the output contract
//! of `intqual --json` and of the MCP server, so field names and `null` handling only change
//! together with `SCHEMA_VERSION`.

use crate::models::ProbeError;
use serde::Serialize;

/// Bumped on any breaking change to the JSON shape below.
pub const SCHEMA_VERSION: u32 = 1;

/// Error kind for a sample that never arrived (its telemetry frame was dropped or the probe
/// stopped). Not a `ProbeError` variant: nothing failed on the wire that we know of.
pub const DROPPED_KIND: &str = "dropped";

/// Where the samples came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Real ICMP/TCP traffic.
    Network,
    /// Deterministic simulated data, no traffic and no DNS.
    Mock,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeasureReport {
    pub schema_version: u32,
    pub target: String,
    pub resolved_ip: String,
    pub port: u16,
    pub count: u32,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub source: Source,
    /// UNIX seconds when the measurement started.
    pub started_at: u64,
    pub duration_ms: u64,
    /// `true` when the caller cancelled; samples not taken are then omitted.
    pub interrupted: bool,
    /// `None` when the ICMP probe was not requested.
    pub icmp: Option<ProbeReport>,
    /// `None` when the TCP probe was not requested.
    pub tcp: Option<ProbeReport>,
    /// `None` when no traceroute was requested.
    pub traceroute: Option<TracerouteReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeReport {
    pub summary: Summary,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub sent: u32,
    pub received: u32,
    pub loss_pct: f64,
    pub min_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    pub seq: u32,
    /// UNIX seconds.
    pub timestamp: u64,
    /// `None` exactly when `error` is set.
    pub rtt_ms: Option<f64>,
    pub error: Option<ErrorInfo>,
}

impl Sample {
    pub fn from_result(seq: u32, timestamp: u64, result: &Result<f64, ProbeError>) -> Self {
        match result {
            Ok(rtt) => Self {
                seq,
                timestamp,
                rtt_ms: Some(*rtt),
                error: None,
            },
            Err(e) => Self {
                seq,
                timestamp,
                rtt_ms: None,
                error: Some(ErrorInfo::from(e)),
            },
        }
    }

    /// A sample whose frame never reached the collector before the deadline.
    pub fn dropped(seq: u32, timestamp: u64) -> Self {
        Self {
            seq,
            timestamp,
            rtt_ms: None,
            error: Some(ErrorInfo {
                kind: DROPPED_KIND.to_string(),
                message: "Sample not received before the deadline".to_string(),
            }),
        }
    }
}

/// Serializable form of a `ProbeError`: match on `kind`, show `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorInfo {
    pub kind: String,
    pub message: String,
}

impl From<&ProbeError> for ErrorInfo {
    fn from(e: &ProbeError) -> Self {
        Self {
            kind: e.kind().to_string(),
            message: e.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TracerouteReport {
    /// The last hop reached the destination.
    pub complete: bool,
    pub error: Option<ErrorInfo>,
    pub hops: Vec<ReportHop>,
}

/// A traceroute hop as reported to callers. Separate from `TracerouteHop`, which is the
/// worker-process IPC contract and has no hostname.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportHop {
    pub hop_number: u8,
    pub ip_address: Option<String>,
    pub hostname: Option<String>,
    pub avg_rtt_ms: Option<f64>,
    pub is_destination: bool,
    pub icmp_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TracerouteHop;
    use serde_json::{Value, json};

    fn summary() -> Summary {
        Summary {
            sent: 2,
            received: 1,
            loss_pct: 50.0,
            min_ms: Some(12.0),
            avg_ms: Some(12.0),
            max_ms: Some(12.0),
            jitter_ms: None,
        }
    }

    fn report() -> MeasureReport {
        let samples = vec![
            Sample::from_result(1, 1_762_250_000, &Ok(12.0)),
            Sample::from_result(2, 1_762_250_000, &Err(ProbeError::IcmpTimeout)),
        ];
        MeasureReport {
            schema_version: SCHEMA_VERSION,
            target: "example.test".to_string(),
            resolved_ip: "192.0.2.1".to_string(),
            port: 443,
            count: 2,
            interval_ms: 500,
            timeout_ms: 1000,
            source: Source::Mock,
            started_at: 1_762_250_000,
            duration_ms: 500,
            interrupted: false,
            icmp: Some(ProbeReport {
                summary: summary(),
                samples: samples.clone(),
            }),
            tcp: Some(ProbeReport {
                summary: summary(),
                samples,
            }),
            traceroute: None,
        }
    }

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        k.sort();
        k
    }

    #[test]
    fn report_json_matches_golden_for_mock_report() {
        let probe = json!({
            "summary": { "sent": 2, "received": 1, "loss_pct": 50.0, "min_ms": 12.0,
                         "avg_ms": 12.0, "max_ms": 12.0, "jitter_ms": null },
            "samples": [
                { "seq": 1, "timestamp": 1_762_250_000u64, "rtt_ms": 12.0, "error": null },
                { "seq": 2, "timestamp": 1_762_250_000u64, "rtt_ms": null,
                  "error": { "kind": "icmp_timeout", "message": "ICMP Timeout" } }
            ]
        });
        let expected = json!({
            "schema_version": 1,
            "target": "example.test",
            "resolved_ip": "192.0.2.1",
            "port": 443,
            "count": 2, "interval_ms": 500, "timeout_ms": 1000,
            "source": "mock",
            "started_at": 1_762_250_000u64,
            "duration_ms": 500,
            "interrupted": false,
            "icmp": probe.clone(),
            "tcp": probe,
            "traceroute": null
        });
        assert_eq!(serde_json::to_value(report()).unwrap(), expected);
    }

    #[test]
    fn report_json_error_object_has_kind_and_message() {
        let s = Sample::from_result(1, 0, &Err(ProbeError::IcmpTimeout));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v["error"],
            json!({ "kind": "icmp_timeout", "message": "ICMP Timeout" })
        );
        assert_eq!(v["rtt_ms"], Value::Null);
    }

    #[test]
    fn report_json_probe_off_serializes_null() {
        let mut r = report();
        r.icmp = None;
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["icmp"], Value::Null);
        assert!(v["tcp"].is_object());
        assert_eq!(serde_json::to_value(Source::Network).unwrap(), "network");
    }

    #[test]
    fn report_json_traceroute_hop_has_hostname_field() {
        let hop = ReportHop {
            hop_number: 1,
            ip_address: Some("192.168.1.1".into()),
            hostname: None,
            avg_rtt_ms: Some(1.2),
            is_destination: false,
            icmp_error: None,
        };
        let v = serde_json::to_value(&hop).unwrap();
        assert_eq!(
            keys(&v),
            [
                "avg_rtt_ms",
                "hop_number",
                "hostname",
                "icmp_error",
                "ip_address",
                "is_destination"
            ]
        );
        assert_eq!(v["hostname"], Value::Null);

        let tr = TracerouteReport {
            complete: true,
            error: None,
            hops: vec![hop],
        };
        let v = serde_json::to_value(&tr).unwrap();
        assert_eq!(keys(&v), ["complete", "error", "hops"]);

        // The IPC contract with the Unix traceroute worker must not change.
        let ipc = TracerouteHop {
            hop_number: 1,
            ip_address: None,
            avg_rtt_ms: None,
            is_destination: false,
            icmp_error: None,
        };
        assert_eq!(
            keys(&serde_json::to_value(&ipc).unwrap()),
            [
                "avg_rtt_ms",
                "hop_number",
                "icmp_error",
                "ip_address",
                "is_destination"
            ]
        );
    }
}
