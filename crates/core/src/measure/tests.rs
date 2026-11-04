//! `measure` tests. Async ones run on paused time (`start_paused`), so intervals and
//! deadlines are exact and nothing waits on a real clock.

use super::*;
use std::sync::Mutex;

const T0: u64 = 1_762_250_000;

fn fixed_clock() -> Result<u64, ProbeError> {
    Ok(T0)
}

fn req(count: u32) -> MeasureRequest {
    MeasureRequest {
        target: "example.test".to_string(),
        port: 443,
        count,
        interval: Duration::from_millis(100),
        timeout: Duration::from_millis(200),
        icmp: true,
        tcp: true,
        traceroute: false,
        max_hops: DEFAULT_MAX_HOPS,
        source: Source::Mock,
    }
}

/// `count × interval + 2 × timeout + 1 s`, the latency collection deadline from the spec.
fn deadline(r: &MeasureRequest) -> Duration {
    r.interval * r.count + r.timeout * 2 + Duration::from_secs(1)
}

async fn run_mock(
    r: &MeasureRequest,
    cancel: CancellationToken,
    progress: Option<mpsc::Sender<Progress>>,
) -> Result<MeasureReport, ProbeError> {
    let sources = MockSources { clock: fixed_clock };
    measure_with(r, cancel, progress, &sources, fixed_clock).await
}

fn errors_of(p: &ProbeReport, kind: &str) -> usize {
    p.samples
        .iter()
        .filter(|s| s.error.as_ref().is_some_and(|e| e.kind == kind))
        .count()
}

// ---- validate ----

fn assert_invalid(r: &MeasureRequest) {
    assert!(
        matches!(r.validate(), Err(ProbeError::InvalidRequest(_))),
        "expected InvalidRequest for {r:?}"
    );
}

#[test]
fn validate_rejects_count_out_of_range() {
    for bad in [0, MAX_COUNT + 1] {
        assert_invalid(&req(bad));
    }
    for good in [1, MAX_COUNT] {
        assert!(req(good).validate().is_ok(), "count {good} should pass");
    }
}

#[test]
fn validate_rejects_interval_out_of_range() {
    let with = |ms: u64| MeasureRequest {
        interval: Duration::from_millis(ms),
        ..req(1)
    };
    assert_invalid(&with(9));
    assert!(with(10).validate().is_ok());
    assert!(with(60_000).validate().is_ok());
    assert_invalid(&with(60_001));
}

#[test]
fn validate_rejects_timeout_out_of_range() {
    let with = |ms: u64| MeasureRequest {
        timeout: Duration::from_millis(ms),
        ..req(1)
    };
    assert_invalid(&with(99));
    assert!(with(100).validate().is_ok());
    assert!(with(30_000).validate().is_ok());
    assert_invalid(&with(30_001));
}

#[test]
fn validate_rejects_max_hops_out_of_range() {
    let with = |hops: u8| MeasureRequest {
        max_hops: hops,
        traceroute: true,
        ..req(1)
    };
    assert_invalid(&with(0));
    assert_invalid(&with(65));
    assert!(with(1).validate().is_ok());
    assert!(with(64).validate().is_ok());
}

#[test]
fn validate_rejects_both_probes_off() {
    let off = MeasureRequest {
        icmp: false,
        tcp: false,
        traceroute: false,
        ..req(1)
    };
    assert_invalid(&off);
    // Traceroute alone is a valid request (the MCP `traceroute` tool).
    let trace_only = MeasureRequest {
        traceroute: true,
        ..off
    };
    assert!(trace_only.validate().is_ok());
}

#[test]
fn validate_rejects_port_zero_when_tcp_on() {
    assert_invalid(&MeasureRequest { port: 0, ..req(1) });
    let no_tcp = MeasureRequest {
        port: 0,
        tcp: false,
        ..req(1)
    };
    assert!(no_tcp.validate().is_ok());
}

// ---- measure on the mock source ----

#[tokio::test(start_paused = true)]
async fn measure_invalid_request_returns_err_without_probing() {
    let start = Instant::now();
    let result = run_mock(&req(0), CancellationToken::new(), None).await;
    assert!(matches!(result, Err(ProbeError::InvalidRequest(_))));
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_returns_exactly_count_samples_per_probe() {
    let report = run_mock(&req(5), CancellationToken::new(), None)
        .await
        .unwrap();
    for probe in [report.icmp.as_ref().unwrap(), report.tcp.as_ref().unwrap()] {
        let seqs: Vec<u32> = probe.samples.iter().map(|s| s.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
        assert_eq!(probe.summary.sent, 5);
        assert_eq!(probe.summary.received, 5);
    }
    assert_eq!(report.source, Source::Mock);
    assert!(report.traceroute.is_none());
    assert!(!report.interrupted);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_icmp_off_reports_null_icmp() {
    let r = MeasureRequest {
        icmp: false,
        ..req(3)
    };
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert!(report.icmp.is_none());
    assert_eq!(report.tcp.unwrap().samples.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_tcp_off_reports_null_tcp() {
    let r = MeasureRequest {
        tcp: false,
        ..req(3)
    };
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert!(report.tcp.is_none());
    assert_eq!(report.icmp.unwrap().samples.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_skips_dns_for_any_target() {
    let r = MeasureRequest {
        target: "nonexistent.invalid".to_string(),
        ..req(1)
    };
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert_eq!(report.resolved_ip, "192.0.2.1");
    assert_eq!(report.target, "nonexistent.invalid");
}

#[tokio::test(start_paused = true)]
async fn measure_mock_is_deterministic() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(4)
    };
    let a = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    let b = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        serde_json::to_value(&b).unwrap()
    );
    assert_eq!(a.started_at, T0);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_waits_interval_between_samples() {
    let r = req(5);
    let start = Instant::now();
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    let elapsed = start.elapsed();
    // The first mock sample is taken at t = 0, so on paused time this is exact.
    assert_eq!(elapsed, r.interval * (r.count - 1));
    assert_eq!(report.duration_ms, elapsed.as_millis() as u64);
}

#[tokio::test(start_paused = true)]
async fn measure_mock_traceroute_returns_three_fixed_hops() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(2)
    };
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    let trace = report.traceroute.unwrap();
    assert!(trace.complete);
    assert!(trace.error.is_none());
    assert_eq!(trace.hops.len(), 3);
    for (i, hop) in trace.hops.iter().enumerate() {
        assert_eq!(hop.hop_number as usize, i + 1);
        let ip: Ipv4Addr = hop.ip_address.as_deref().unwrap().parse().unwrap();
        assert_eq!(&ip.octets()[..3], &[192, 0, 2]);
        assert!(hop.hostname.is_some());
    }
    let last = trace.hops.last().unwrap();
    assert!(last.is_destination);
    assert_eq!(last.ip_address.as_deref(), Some("192.0.2.1"));
    assert!(trace.hops[..2].iter().all(|h| !h.is_destination));
}

#[tokio::test(start_paused = true)]
async fn measure_not_cancelled_reports_interrupted_false() {
    let report = run_mock(&req(2), CancellationToken::new(), None)
        .await
        .unwrap();
    assert!(!report.interrupted);
}

#[tokio::test(start_paused = true)]
async fn measure_cancel_returns_partial_report_interrupted() {
    let r = req(10);
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    // Mock samples arrive at 0, 100 and 200 ms; cancel before the 4th at 300 ms.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        canceller.cancel();
    });
    let report = run_mock(&r, cancel, None).await.unwrap();
    assert!(report.interrupted);
    for probe in [report.icmp.unwrap(), report.tcp.unwrap()] {
        assert_eq!(probe.samples.len(), 3);
        assert_eq!(probe.summary.sent, 3);
        assert_eq!(errors_of(&probe, DROPPED_KIND), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn measure_progress_reports_each_tick_and_hop() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(3)
    };
    let (tx, mut rx) = mpsc::channel(100);
    run_mock(&r, CancellationToken::new(), Some(tx))
        .await
        .unwrap();
    let mut got = Vec::new();
    while let Ok(p) = rx.try_recv() {
        got.push(p);
    }
    let latency = |done| Progress {
        phase: Phase::Latency,
        done,
        total: 3,
    };
    let hop = |done| Progress {
        phase: Phase::Traceroute,
        done,
        total: u32::from(DEFAULT_MAX_HOPS),
    };
    // The mock stops at the destination after 3 hops: no faked `done == total`.
    assert_eq!(
        got,
        [latency(1), latency(2), latency(3), hop(1), hop(2), hop(3)]
    );
}

#[tokio::test(start_paused = true)]
async fn measure_progress_full_channel_does_not_block() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(5)
    };
    let (tx, rx) = mpsc::channel(1); // never read
    let start = Instant::now();
    let report = run_mock(&r, CancellationToken::new(), Some(tx))
        .await
        .unwrap();
    assert!(start.elapsed() < deadline(&r));
    assert_eq!(report.icmp.unwrap().samples.len(), 5);
    // 8 updates were due (5 ticks + 3 hops); all but the first were dropped, not queued.
    assert_eq!(rx.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn measure_progress_none_is_accepted() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(3)
    };
    let (tx, _rx) = mpsc::channel(100);
    let with = run_mock(&r, CancellationToken::new(), Some(tx))
        .await
        .unwrap();
    let without = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert_eq!(with, without);
}

// ---- injected sources: stalls and failures without sockets ----

#[derive(Clone, Copy)]
enum LatencyMode {
    Mock,
    /// Sends seq 1 for each probe, then holds the channel open and sends nothing more.
    StallAfterFirst,
}

#[derive(Clone, Copy)]
enum TraceMode {
    Mock,
    /// One hop, then `PermissionDenied` (Unix without root).
    FailAfterOne,
    /// Two hops, then waits for cancel.
    StallAfterTwo,
}

struct TestSources {
    mock: MockSources,
    latency: LatencyMode,
    trace: TraceMode,
    /// The latency probes' token, to check they were stopped before the traceroute started.
    latency_token: Mutex<Option<CancellationToken>>,
    /// Set by `traceroute()`: was the latency token already cancelled?
    latency_stopped_before_trace: Mutex<Option<bool>>,
}

impl TestSources {
    fn new(latency: LatencyMode, trace: TraceMode) -> Self {
        Self {
            mock: MockSources { clock: fixed_clock },
            latency,
            trace,
            latency_token: Mutex::new(None),
            latency_stopped_before_trace: Mutex::new(None),
        }
    }
}

fn hop(n: u8) -> TracerouteHop {
    TracerouteHop {
        hop_number: n,
        ip_address: Some(format!("192.0.2.{n}")),
        avg_rtt_ms: Some(1.0),
        is_destination: false,
        icmp_error: None,
    }
}

impl Sources for TestSources {
    async fn resolve(&self, req: &MeasureRequest) -> Result<SocketAddr, ProbeError> {
        self.mock.resolve(req).await
    }

    fn start_latency(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) {
        *self.latency_token.lock().unwrap() = Some(cancel.clone());
        match self.latency {
            LatencyMode::Mock => self.mock.start_latency(addr, req, tx, cancel),
            LatencyMode::StallAfterFirst => {
                tokio::spawn(async move {
                    let ping = TelemetryEvent::Ping {
                        sequence_number: 1,
                        target_ip: String::new(),
                        result: Ok(10.0),
                        timestamp: T0,
                    };
                    let tcp = TelemetryEvent::Tcp {
                        sequence_number: 1,
                        target_ip: String::new(),
                        result: Ok(12.0),
                        timestamp: T0,
                    };
                    let _ = tx.send(ping).await;
                    let _ = tx.send(tcp).await;
                    cancel.cancelled().await;
                    drop(tx);
                });
            }
        }
    }

    fn traceroute(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send + 'static {
        let stopped = self
            .latency_token
            .lock()
            .unwrap()
            .as_ref()
            .map(CancellationToken::is_cancelled);
        *self.latency_stopped_before_trace.lock().unwrap() = stopped;
        let mock = self.mock.traceroute(addr, req, tx.clone(), cancel.clone());
        let mode = self.trace;
        async move {
            match mode {
                TraceMode::Mock => mock.await,
                TraceMode::FailAfterOne => {
                    let _ = tx.send(TelemetryEvent::TracerouteHop(hop(1))).await;
                    Err(ProbeError::PermissionDenied)
                }
                TraceMode::StallAfterTwo => {
                    let _ = tx.send(TelemetryEvent::TracerouteHop(hop(1))).await;
                    let _ = tx.send(TelemetryEvent::TracerouteHop(hop(2))).await;
                    cancel.cancelled().await;
                    Ok(())
                }
            }
        }
    }

    async fn reverse_dns(&self, ip: IpAddr) -> Option<String> {
        self.mock.reverse_dns(ip).await
    }
}

async fn run_with(
    r: &MeasureRequest,
    sources: &TestSources,
    cancel: CancellationToken,
) -> Result<MeasureReport, ProbeError> {
    measure_with(r, cancel, None, sources, fixed_clock).await
}

/// A start time different from the `T0` the test sources stamp on their samples, so a test
/// can tell `started_at` apart from a sample timestamp.
fn later_clock() -> Result<u64, ProbeError> {
    Ok(T0 + 100)
}

#[tokio::test(start_paused = true)]
async fn measure_deadline_records_missing_samples_as_dropped() {
    let r = req(3);
    let sources = TestSources::new(LatencyMode::StallAfterFirst, TraceMode::Mock);
    let start = Instant::now();
    let report = measure_with(&r, CancellationToken::new(), None, &sources, later_clock)
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(report.started_at, T0 + 100);
    assert!(
        elapsed >= deadline(&r),
        "returned before the deadline: {elapsed:?}"
    );
    assert!(
        elapsed < deadline(&r) + Duration::from_millis(50),
        "overshot the deadline: {elapsed:?}"
    );
    assert!(!report.interrupted);
    for probe in [report.icmp.unwrap(), report.tcp.unwrap()] {
        assert_eq!(probe.summary.sent, 3);
        assert_eq!(probe.summary.received, 1);
        assert_eq!(errors_of(&probe, DROPPED_KIND), 2);
        let missing: Vec<&Sample> = probe
            .samples
            .iter()
            .filter(|s| s.rtt_ms.is_none())
            .collect();
        assert_eq!(missing.iter().map(|s| s.seq).collect::<Vec<_>>(), [2, 3]);
        assert!(missing.iter().all(|s| s.timestamp == report.started_at));
        assert_eq!(
            probe.samples[0].timestamp, T0,
            "received sample keeps its own time"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn measure_traceroute_failure_is_data_not_err() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(2)
    };
    let sources = TestSources::new(LatencyMode::Mock, TraceMode::FailAfterOne);
    let report = run_with(&r, &sources, CancellationToken::new())
        .await
        .unwrap();
    let trace = report.traceroute.unwrap();
    assert_eq!(
        trace.error.map(|e| e.kind),
        Some("permission_denied".to_string())
    );
    assert_eq!(trace.hops.len(), 1);
    assert!(!trace.complete);
    assert!(!report.interrupted);
    assert_eq!(report.icmp.unwrap().samples.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn measure_cancel_during_traceroute_keeps_hops_incomplete() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(2)
    };
    let sources = TestSources::new(LatencyMode::Mock, TraceMode::StallAfterTwo);
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    // Latency is done at ~100 ms; the stalled traceroute holds until this fires.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        canceller.cancel();
    });
    let report = run_with(&r, &sources, cancel).await.unwrap();
    assert!(report.interrupted);
    assert_eq!(report.icmp.unwrap().samples.len(), 2);
    let trace = report.traceroute.unwrap();
    assert_eq!(trace.hops.len(), 2);
    assert!(!trace.complete);
    assert!(trace.error.is_none());
}

#[tokio::test(start_paused = true)]
async fn measure_traceroute_runs_after_ping_tcp() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(4)
    };
    let sources = TestSources::new(LatencyMode::Mock, TraceMode::Mock);
    let report = run_with(&r, &sources, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        *sources.latency_stopped_before_trace.lock().unwrap(),
        Some(true),
        "traceroute started while the latency probes were still running"
    );
    assert_eq!(report.icmp.unwrap().samples.len(), 4);
    assert_eq!(report.traceroute.unwrap().hops.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn measure_cancel_during_latency_skips_traceroute() {
    let r = MeasureRequest {
        traceroute: true,
        ..req(10)
    };
    let sources = TestSources::new(LatencyMode::Mock, TraceMode::Mock);
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        canceller.cancel();
    });
    let report = run_with(&r, &sources, cancel).await.unwrap();
    assert!(report.interrupted);
    assert_eq!(report.icmp.unwrap().samples.len(), 3);
    let trace = report.traceroute.unwrap();
    assert!(trace.hops.is_empty());
    assert!(!trace.complete);
    assert!(trace.error.is_none());
    assert_eq!(
        *sources.latency_stopped_before_trace.lock().unwrap(),
        None,
        "the traceroute source must not be started after a cancel"
    );
}

#[tokio::test(start_paused = true)]
async fn measure_traceroute_only_reports_null_icmp_tcp() {
    let r = MeasureRequest {
        icmp: false,
        tcp: false,
        traceroute: true,
        ..req(1)
    };
    let report = run_mock(&r, CancellationToken::new(), None).await.unwrap();
    assert!(report.icmp.is_none());
    assert!(report.tcp.is_none());
    assert!(!report.interrupted);
    let trace = report.traceroute.unwrap();
    assert!(trace.complete);
    assert_eq!(trace.hops.len(), 3);
}

// ---- real DNS, offline-safe ----

/// `.invalid` is reserved (RFC 6761) and never resolves, online or offline.
#[tokio::test]
async fn measure_unresolvable_target_returns_dns_error() {
    let r = MeasureRequest {
        target: "nonexistent.invalid".to_string(),
        source: Source::Network,
        ..req(1)
    };
    let result = measure(&r, CancellationToken::new(), None).await;
    assert!(
        matches!(result, Err(ProbeError::DnsResolution(_))),
        "got {result:?}"
    );
}
