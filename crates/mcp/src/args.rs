//! Tool arguments and their mapping to a `MeasureRequest`.
//!
//! The MCP limits are stricter than the CLI's: a model can call a tool in a loop and the host
//! puts its own timeout on every request, so each call must stay short and bounded. The checks
//! here run before `MeasureRequest::validate()`, so an out-of-range value gets the MCP bound in
//! its message, not the wider CLI one. Pure functions: tested without a transport.

use intqual_core::measure::{MeasureRequest, Source};
use intqual_core::models::ProbeError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::ops::RangeInclusive;
use std::time::Duration;

pub const COUNT_RANGE: RangeInclusive<u32> = 1..=20;
pub const INTERVAL_MS_RANGE: RangeInclusive<u64> = 100..=5000;
pub const TIMEOUT_MS_RANGE: RangeInclusive<u64> = 100..=5000;
pub const MAX_HOPS_RANGE: RangeInclusive<u8> = 1..=64;
/// Worst-case duration of one tool call. Hosts time requests out on their own; staying well
/// under a minute keeps a call from being abandoned half-way.
pub const CALL_BUDGET: Duration = Duration::from_secs(60);

const DEFAULT_PORT: u16 = 443;
const DEFAULT_COUNT: u32 = 4;
const DEFAULT_INTERVAL_MS: u64 = 500;
const DEFAULT_TIMEOUT_MS: u64 = 1000;
const DEFAULT_TRACEROUTE_TIMEOUT_MS: u64 = 500;
const DEFAULT_MAX_HOPS: u8 = 30;
/// Traceroute probes per hop (see `TracerouteProbe`), used for the time budget.
const PROBES_PER_HOP: u32 = 3;

fn default_port() -> u16 {
    DEFAULT_PORT
}
fn default_count() -> u32 {
    DEFAULT_COUNT
}
fn default_interval_ms() -> u64 {
    DEFAULT_INTERVAL_MS
}
fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}
fn default_traceroute_timeout_ms() -> u64 {
    DEFAULT_TRACEROUTE_TIMEOUT_MS
}
fn default_max_hops() -> u8 {
    DEFAULT_MAX_HOPS
}

/// Arguments of the `ping` tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct PingArgs {
    /// Hostname or IP address to measure, for example `example.com` or `1.1.1.1`.
    pub target: String,
    /// Number of ICMP echo requests, 1 to 20.
    #[serde(default = "default_count")]
    #[schemars(range(min = 1, max = 20))]
    pub count: u32,
    /// Milliseconds between requests, 100 to 5000.
    #[serde(default = "default_interval_ms")]
    #[schemars(range(min = 100, max = 5000))]
    pub interval_ms: u64,
    /// Milliseconds to wait for each reply, 100 to 5000.
    #[serde(default = "default_timeout_ms")]
    #[schemars(range(min = 100, max = 5000))]
    pub timeout_ms: u64,
}

/// Arguments of the `tcp_latency` tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct TcpArgs {
    /// Hostname or IP address to measure, for example `example.com` or `1.1.1.1`.
    pub target: String,
    /// TCP port to connect to, 1 to 65535. 443 (HTTPS) passes most firewalls.
    #[serde(default = "default_port")]
    #[schemars(range(min = 1, max = 65535))]
    pub port: u16,
    /// Number of TCP connects, 1 to 20.
    #[serde(default = "default_count")]
    #[schemars(range(min = 1, max = 20))]
    pub count: u32,
    /// Milliseconds between connects, 100 to 5000.
    #[serde(default = "default_interval_ms")]
    #[schemars(range(min = 100, max = 5000))]
    pub interval_ms: u64,
    /// Milliseconds to wait for each handshake, 100 to 5000.
    #[serde(default = "default_timeout_ms")]
    #[schemars(range(min = 100, max = 5000))]
    pub timeout_ms: u64,
}

/// Arguments of the `traceroute` tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct TracerouteArgs {
    /// Hostname or IP address to trace, for example `example.com` or `1.1.1.1`.
    pub target: String,
    /// Highest TTL to probe, 1 to 64.
    #[serde(default = "default_max_hops")]
    #[schemars(range(min = 1, max = 64))]
    pub max_hops: u8,
    /// Milliseconds to wait for each probe, 100 to 5000. Three probes are sent per hop.
    #[serde(default = "default_traceroute_timeout_ms")]
    #[schemars(range(min = 100, max = 5000))]
    pub timeout_ms: u64,
}

/// Deserializes raw tool arguments. A wrong type or a missing field is an `invalid_request`
/// tool error, not a protocol error, so the model sees the reason and can retry.
pub fn parse<T: DeserializeOwned>(
    args: serde_json::Map<String, serde_json::Value>,
) -> Result<T, ProbeError> {
    serde_json::from_value(serde_json::Value::Object(args))
        .map_err(|e| invalid(format!("invalid arguments: {e}")))
}

fn invalid(msg: String) -> ProbeError {
    ProbeError::InvalidRequest(msg)
}

fn check_count(count: u32) -> Result<(), ProbeError> {
    if COUNT_RANGE.contains(&count) {
        return Ok(());
    }
    Err(invalid(format!(
        "count must be between {} and {}, got {count}",
        COUNT_RANGE.start(),
        COUNT_RANGE.end()
    )))
}

fn check_ms(name: &str, value: u64, range: &RangeInclusive<u64>) -> Result<(), ProbeError> {
    if range.contains(&value) {
        return Ok(());
    }
    Err(invalid(format!(
        "{name} must be between {} and {}, got {value}",
        range.start(),
        range.end()
    )))
}

fn check_max_hops(max_hops: u8) -> Result<(), ProbeError> {
    if MAX_HOPS_RANGE.contains(&max_hops) {
        return Ok(());
    }
    Err(invalid(format!(
        "max_hops must be between {} and {}, got {max_hops}",
        MAX_HOPS_RANGE.start(),
        MAX_HOPS_RANGE.end()
    )))
}

fn check_port(port: u16) -> Result<(), ProbeError> {
    if port == 0 {
        return Err(invalid(
            "port must be between 1 and 65535, got 0".to_string(),
        ));
    }
    Ok(())
}

/// The latency tools' worst case, the same bound `measure` uses as its collection deadline:
/// `count × interval + 2 × timeout + 1 s`.
pub fn latency_bound(count: u32, interval: Duration, timeout: Duration) -> Duration {
    interval * count + timeout * 2 + Duration::from_secs(1)
}

/// The traceroute worst case: every hop times out on all its probes.
pub fn traceroute_bound(max_hops: u8, timeout: Duration) -> Duration {
    timeout * (u32::from(max_hops) * PROBES_PER_HOP)
}

/// Rejects a call whose worst case is over `CALL_BUDGET` (exactly the budget is accepted). The
/// message states both numbers so the model can retry with smaller arguments.
fn check_budget(bound: Duration, formula: &str) -> Result<(), ProbeError> {
    if bound <= CALL_BUDGET {
        return Ok(());
    }
    Err(invalid(format!(
        "worst-case duration {} ms ({formula}) is over the {} s limit per call; \
         use a smaller count, interval or timeout",
        bound.as_millis(),
        CALL_BUDGET.as_secs()
    )))
}

fn latency_request(
    target: &str,
    port: u16,
    count: u32,
    interval_ms: u64,
    timeout_ms: u64,
    source: Source,
) -> Result<MeasureRequest, ProbeError> {
    check_count(count)?;
    check_ms("interval_ms", interval_ms, &INTERVAL_MS_RANGE)?;
    check_ms("timeout_ms", timeout_ms, &TIMEOUT_MS_RANGE)?;
    let interval = Duration::from_millis(interval_ms);
    let timeout = Duration::from_millis(timeout_ms);
    check_budget(
        latency_bound(count, interval, timeout),
        "count × interval_ms + 2 × timeout_ms + 1000 ms",
    )?;
    let req = MeasureRequest {
        target: target.to_string(),
        port,
        count,
        interval,
        timeout,
        icmp: false,
        tcp: false,
        traceroute: false,
        max_hops: DEFAULT_MAX_HOPS,
        source,
    };
    Ok(req)
}

/// `ping`: ICMP only.
pub fn ping_request(args: &PingArgs, source: Source) -> Result<MeasureRequest, ProbeError> {
    let mut req = latency_request(
        &args.target,
        DEFAULT_PORT,
        args.count,
        args.interval_ms,
        args.timeout_ms,
        source,
    )?;
    req.icmp = true;
    req.validate()?;
    Ok(req)
}

/// `tcp_latency`: TCP only.
pub fn tcp_request(args: &TcpArgs, source: Source) -> Result<MeasureRequest, ProbeError> {
    check_port(args.port)?;
    let mut req = latency_request(
        &args.target,
        args.port,
        args.count,
        args.interval_ms,
        args.timeout_ms,
        source,
    )?;
    req.tcp = true;
    req.validate()?;
    Ok(req)
}

/// `traceroute`: traceroute only. `count` and `interval` are unused by the traceroute but
/// must pass `validate()`, so they get the smallest valid values (the report shows `count: 1`).
pub fn traceroute_request(
    args: &TracerouteArgs,
    source: Source,
) -> Result<MeasureRequest, ProbeError> {
    check_max_hops(args.max_hops)?;
    check_ms("timeout_ms", args.timeout_ms, &TIMEOUT_MS_RANGE)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    check_budget(
        traceroute_bound(args.max_hops, timeout),
        "max_hops × 3 × timeout_ms",
    )?;
    let req = MeasureRequest {
        target: args.target.clone(),
        port: DEFAULT_PORT,
        count: 1,
        interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
        timeout,
        icmp: false,
        tcp: false,
        traceroute: true,
        max_hops: args.max_hops,
        source,
    };
    req.validate()?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ping(count: u32, interval_ms: u64, timeout_ms: u64) -> PingArgs {
        PingArgs {
            target: "example.test".into(),
            count,
            interval_ms,
            timeout_ms,
        }
    }

    fn tcp(port: u16) -> TcpArgs {
        TcpArgs {
            target: "example.test".into(),
            port,
            count: DEFAULT_COUNT,
            interval_ms: DEFAULT_INTERVAL_MS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    fn trace(max_hops: u8, timeout_ms: u64) -> TracerouteArgs {
        TracerouteArgs {
            target: "example.test".into(),
            max_hops,
            timeout_ms,
        }
    }

    fn from_json<T: for<'de> Deserialize<'de>>(v: serde_json::Value) -> T {
        serde_json::from_value(v).unwrap()
    }

    fn message(r: Result<MeasureRequest, ProbeError>) -> String {
        match r {
            Err(e @ ProbeError::InvalidRequest(_)) => {
                assert_eq!(e.kind(), "invalid_request");
                e.to_string()
            }
            other => panic!("expected invalid_request, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_wrong_type_and_missing_target_as_invalid_request() {
        let wrong = serde_json::json!({ "target": "x", "count": "x" });
        let Err(e) = parse::<PingArgs>(wrong.as_object().unwrap().clone()) else {
            panic!("count \"x\" must not parse");
        };
        assert_eq!(e.kind(), "invalid_request");
        assert!(e.to_string().contains("invalid arguments"), "{e}");
        let missing = serde_json::json!({ "max_hops": 5 });
        let Err(e) = parse::<TracerouteArgs>(missing.as_object().unwrap().clone()) else {
            panic!("missing target must not parse");
        };
        assert!(e.to_string().contains("target"), "{e}");
    }

    #[test]
    fn ping_args_default_map_to_icmp_only_request() {
        let args: PingArgs = from_json(serde_json::json!({ "target": "example.test" }));
        let req = ping_request(&args, Source::Mock).unwrap();
        assert_eq!(req.target, "example.test");
        assert_eq!(req.count, 4);
        assert_eq!(req.interval, Duration::from_millis(500));
        assert_eq!(req.timeout, Duration::from_millis(1000));
        assert!(req.icmp && !req.tcp && !req.traceroute);
        assert_eq!(req.source, Source::Mock);
    }

    #[test]
    fn tcp_args_default_map_to_tcp_only_with_port_443() {
        let args: TcpArgs = from_json(serde_json::json!({ "target": "example.test" }));
        let req = tcp_request(&args, Source::Network).unwrap();
        assert_eq!(req.port, 443);
        assert_eq!(req.count, 4);
        assert_eq!(req.interval, Duration::from_millis(500));
        assert_eq!(req.timeout, Duration::from_millis(1000));
        assert!(!req.icmp && req.tcp && !req.traceroute);
        assert_eq!(req.source, Source::Network);
    }

    #[test]
    fn tcp_args_explicit_port_is_passed_through() {
        let req = tcp_request(&tcp(8080), Source::Mock).unwrap();
        assert_eq!(req.port, 8080);
    }

    #[test]
    fn tcp_args_port_zero_rejected() {
        let text = message(tcp_request(&tcp(0), Source::Mock));
        assert!(text.contains("port"), "{text}");
    }

    #[test]
    fn traceroute_args_default_map_to_traceroute_only() {
        let args: TracerouteArgs = from_json(serde_json::json!({ "target": "example.test" }));
        let req = traceroute_request(&args, Source::Mock).unwrap();
        assert_eq!(req.max_hops, 30);
        assert_eq!(req.timeout, Duration::from_millis(500));
        assert!(!req.icmp && !req.tcp && req.traceroute);
    }

    #[test]
    fn limits_accept_count_1_and_20_reject_0_and_21() {
        assert!(ping_request(&ping(1, 500, 1000), Source::Mock).is_ok());
        assert!(ping_request(&ping(20, 500, 1000), Source::Mock).is_ok());
        assert!(message(ping_request(&ping(0, 500, 1000), Source::Mock)).contains("count"));
        assert!(message(ping_request(&ping(21, 500, 1000), Source::Mock)).contains("count"));
    }

    #[test]
    fn limits_interval_100_and_5000_pass_99_and_5001_fail() {
        assert!(ping_request(&ping(1, 100, 1000), Source::Mock).is_ok());
        assert!(ping_request(&ping(1, 5000, 1000), Source::Mock).is_ok());
        assert!(message(ping_request(&ping(1, 99, 1000), Source::Mock)).contains("interval_ms"));
        assert!(message(ping_request(&ping(1, 5001, 1000), Source::Mock)).contains("interval_ms"));
    }

    #[test]
    fn limits_timeout_100_and_5000_pass_99_and_5001_fail() {
        assert!(ping_request(&ping(1, 500, 100), Source::Mock).is_ok());
        assert!(ping_request(&ping(1, 500, 5000), Source::Mock).is_ok());
        assert!(message(ping_request(&ping(1, 500, 99), Source::Mock)).contains("timeout_ms"));
        assert!(message(ping_request(&ping(1, 500, 5001), Source::Mock)).contains("timeout_ms"));
        // Traceroute has its own timeout argument with the same bounds.
        assert!(traceroute_request(&trace(1, 100), Source::Mock).is_ok());
        assert!(traceroute_request(&trace(1, 5000), Source::Mock).is_ok());
        assert!(message(traceroute_request(&trace(1, 99), Source::Mock)).contains("timeout_ms"));
        assert!(message(traceroute_request(&trace(1, 5001), Source::Mock)).contains("timeout_ms"));
    }

    #[test]
    fn limits_max_hops_1_and_64_pass_0_and_65_fail() {
        assert!(traceroute_request(&trace(1, 100), Source::Mock).is_ok());
        assert!(traceroute_request(&trace(64, 100), Source::Mock).is_ok());
        assert!(message(traceroute_request(&trace(0, 100), Source::Mock)).contains("max_hops"));
        assert!(message(traceroute_request(&trace(65, 100), Source::Mock)).contains("max_hops"));
    }

    #[test]
    fn budget_latency_exactly_60s_passes_and_60s_plus_1ms_fails() {
        // 10 × 5000 + 2 × 4500 + 1000 = 60 000 ms.
        assert_eq!(
            latency_bound(10, Duration::from_millis(5000), Duration::from_millis(4500)),
            CALL_BUDGET
        );
        assert!(ping_request(&ping(10, 5000, 4500), Source::Mock).is_ok());
        // 19 × 2999 + 2 × 1010 + 1000 = 60 001 ms.
        assert_eq!(
            latency_bound(19, Duration::from_millis(2999), Duration::from_millis(1010)),
            CALL_BUDGET + Duration::from_millis(1)
        );
        let text = message(ping_request(&ping(19, 2999, 1010), Source::Mock));
        assert!(text.contains("60001 ms"), "{text}");
    }

    #[test]
    fn budget_extreme_latency_20x5000_rejected_with_bound_in_text() {
        let text = message(ping_request(&ping(20, 5000, 5000), Source::Mock));
        assert!(text.contains("111000 ms"), "{text}");
        assert!(text.contains("60 s"), "{text}");
        let mut args = tcp(443);
        (args.count, args.interval_ms, args.timeout_ms) = (20, 5000, 5000);
        assert!(message(tcp_request(&args, Source::Mock)).contains("60 s"));
    }

    #[test]
    fn budget_traceroute_boundary_cases() {
        let over = message(traceroute_request(&trace(30, 1000), Source::Mock));
        assert!(over.contains("90000 ms") && over.contains("60 s"), "{over}");
        assert!(traceroute_request(&trace(30, 500), Source::Mock).is_ok());
        assert_eq!(
            traceroute_bound(40, Duration::from_millis(500)),
            CALL_BUDGET
        );
        assert!(traceroute_request(&trace(40, 500), Source::Mock).is_ok());
        assert!(message(traceroute_request(&trace(64, 500), Source::Mock)).contains("60 s"));
    }

    #[test]
    fn limit_check_runs_before_measure_validate() {
        // `validate()` accepts count 21 and interval 50 ms; the MCP limits do not, and their
        // message names the MCP bound.
        let count = message(ping_request(&ping(21, 500, 1000), Source::Mock));
        assert!(count.contains("between 1 and 20"), "{count}");
        let interval = message(ping_request(&ping(1, 50, 1000), Source::Mock));
        assert!(interval.contains("between 100 and 5000"), "{interval}");
        let hops = message(traceroute_request(&trace(0, 500), Source::Mock));
        assert!(hops.contains("between 1 and 64"), "{hops}");
    }
}
