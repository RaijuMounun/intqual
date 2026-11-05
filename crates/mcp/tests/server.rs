//! In-process tests: `IntqualServer` served over `tokio::io::duplex`, driven by an rmcp client.
//! No admin rights and no internet: `Source::Mock`, fake measurers, and loopback only. The
//! one `Source::Network` DNS case uses `.invalid` (RFC 6761), which never resolves.
//!
//! Spike result (test plan "First: spike"): `start_paused = true` works across the duplex and
//! the rmcp client, so the (R) rows use paused time. rmcp 3.8 does not drop a cancelled
//! handler's future; it cancels `ctx.ct` and drops the response (see `server.rs`).

use intqual_core::measure::{
    ErrorInfo, MeasureReport, MeasureRequest, ProbeReport, Progress, Sample, Source, summarize,
};
use intqual_core::models::ProbeError;
use intqual_mcp::{CoreMeasurer, IntqualServer, Measurer};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientRequest, ProgressNotificationParam, Request,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

type ServerTask = JoinHandle<Result<rmcp::service::QuitReason, String>>;

/// Client that forwards every progress notification to a channel.
#[derive(Clone)]
struct ProgressClient {
    tx: mpsc::UnboundedSender<ProgressNotificationParam>,
}

impl ClientHandler for ProgressClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.tx.send(params);
    }
}

fn spawn_server<M: Measurer>(server: IntqualServer<M>, io: tokio::io::DuplexStream) -> ServerTask {
    tokio::spawn(async move {
        let running = server.serve(io).await.map_err(|e| e.to_string())?;
        running.waiting().await.map_err(|e| e.to_string())
    })
}

async fn connect<M: Measurer>(
    server: IntqualServer<M>,
) -> (RunningService<RoleClient, ()>, ServerTask) {
    let (s, c) = tokio::io::duplex(64 * 1024);
    let task = spawn_server(server, s);
    let client = ().serve(c).await.expect("client handshake");
    (client, task)
}

async fn connect_progress<M: Measurer>(
    server: IntqualServer<M>,
) -> (
    RunningService<RoleClient, ProgressClient>,
    mpsc::UnboundedReceiver<ProgressNotificationParam>,
    ServerTask,
) {
    let (s, c) = tokio::io::duplex(64 * 1024);
    let task = spawn_server(server, s);
    let (tx, rx) = mpsc::unbounded_channel();
    let client = ProgressClient { tx }
        .serve(c)
        .await
        .expect("client handshake");
    (client, rx, task)
}

fn mock() -> IntqualServer {
    IntqualServer::new(Source::Mock)
}

fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let Value::Object(map) = args else {
        panic!("arguments must be an object")
    };
    CallToolRequestParams::new(name).with_arguments(map)
}

fn report(result: &CallToolResult) -> MeasureReport {
    assert_eq!(
        result.is_error,
        Some(false),
        "tool error: {:?}",
        result.content
    );
    let structured = result
        .structured_content
        .clone()
        .expect("structured content");
    serde_json::from_value(structured).expect("structured content is a MeasureReport")
}

/// `isError` result's single `{kind, message}` block.
fn tool_error(result: &CallToolResult) -> ErrorInfo {
    assert_eq!(result.is_error, Some(true), "expected a tool error");
    assert_eq!(result.content.len(), 1);
    let text = &result.content[0].as_text().expect("text block").text;
    serde_json::from_str(text).expect("error block is {kind, message}")
}

/// Wraps a measurer and records how it was used.
#[derive(Clone, Default)]
struct Stats {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

struct Recording<M> {
    inner: M,
    stats: Stats,
    /// `interrupted` of every report the inner measurer returned.
    done: mpsc::UnboundedSender<bool>,
}

impl<M: Measurer> Measurer for Recording<M> {
    async fn measure(
        &self,
        req: MeasureRequest,
        cancel: CancellationToken,
        progress: Option<mpsc::Sender<Progress>>,
    ) -> Result<MeasureReport, ProbeError> {
        self.stats.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.stats.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.stats.max_active.fetch_max(now, Ordering::SeqCst);
        let result = self.inner.measure(req, cancel, progress).await;
        self.stats.active.fetch_sub(1, Ordering::SeqCst);
        let _ = self.done.send(result.as_ref().is_ok_and(|r| r.interrupted));
        result
    }
}

fn recording() -> (
    IntqualServer<Recording<CoreMeasurer>>,
    Stats,
    mpsc::UnboundedReceiver<bool>,
) {
    let stats = Stats::default();
    let (done, rx) = mpsc::unbounded_channel();
    let measurer = Recording {
        inner: CoreMeasurer,
        stats: stats.clone(),
        done,
    };
    (
        IntqualServer::with_measurer(Source::Mock, measurer),
        stats,
        rx,
    )
}

/// Every sample fails: a 100 % loss report.
struct AllTimeouts;

impl Measurer for AllTimeouts {
    async fn measure(
        &self,
        req: MeasureRequest,
        _cancel: CancellationToken,
        _progress: Option<mpsc::Sender<Progress>>,
    ) -> Result<MeasureReport, ProbeError> {
        let samples: Vec<Sample> = (1..=req.count)
            .map(|seq| Sample::from_result(seq, 0, &Err(ProbeError::IcmpTimeout)))
            .collect();
        Ok(MeasureReport {
            schema_version: intqual_core::measure::SCHEMA_VERSION,
            target: req.target.clone(),
            resolved_ip: "192.0.2.1".into(),
            ip_version: intqual_core::IpFamily::V4,
            port: req.port,
            count: req.count,
            interval_ms: req.interval.as_millis() as u64,
            timeout_ms: req.timeout.as_millis() as u64,
            source: req.source,
            started_at: 0,
            duration_ms: 0,
            interrupted: false,
            icmp: Some(ProbeReport {
                summary: summarize(&samples),
                samples,
            }),
            tcp: None,
            traceroute: None,
        })
    }
}

/// Ignores the cancel token on its first call and never returns; normal mock afterwards.
struct StallOnce {
    calls: AtomicUsize,
}

impl Measurer for StallOnce {
    fn measure(
        &self,
        req: MeasureRequest,
        cancel: CancellationToken,
        progress: Option<mpsc::Sender<Progress>>,
    ) -> impl Future<Output = Result<MeasureReport, ProbeError>> + Send {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        async move {
            if first {
                std::future::pending::<()>().await;
            }
            CoreMeasurer.measure(req, cancel, progress).await
        }
    }
}

// ---------- tool listing and server info ----------

#[tokio::test]
async fn list_tools_returns_exactly_three_tools() {
    let (client, _server) = connect(mock()).await;
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["ping", "tcp_latency", "traceroute"]);
}

#[tokio::test]
async fn list_tools_have_input_and_output_schema() {
    let (client, _server) = connect(mock()).await;
    for tool in client.list_all_tools().await.unwrap() {
        let input = Value::Object((*tool.input_schema).clone());
        assert_eq!(input["required"], json!(["target"]), "{}", tool.name);
        let props = input["properties"].as_object().expect("properties");
        assert!(props.contains_key("target"));
        for (arg, schema) in props {
            let desc = schema["description"].as_str().unwrap_or_default();
            assert!(!desc.is_empty(), "{}.{arg} has no description", tool.name);
        }
        let output = Value::Object((*tool.output_schema.clone().expect("outputSchema")).clone());
        let out_props = output["properties"].as_object().expect("output properties");
        for key in ["schema_version", "icmp", "tcp", "traceroute", "interrupted"] {
            assert!(
                out_props.contains_key(key),
                "{} output lacks {key}",
                tool.name
            );
        }
    }
}

#[tokio::test]
async fn input_schema_ranges_match_limit_constants() {
    use intqual_mcp::args::{
        COUNT_RANGE, INTERVAL_MS_RANGE, MAX_HOPS_RANGE, PORT_RANGE, TIMEOUT_MS_RANGE,
    };
    let (client, _server) = connect(mock()).await;
    let mut checked = 0;
    let mut ip_versions = 0;
    for tool in client.list_all_tools().await.unwrap() {
        let input = Value::Object((*tool.input_schema).clone());
        assert_eq!(
            input["additionalProperties"], false,
            "{} accepts unknown args",
            tool.name
        );
        for (arg, schema) in input["properties"].as_object().expect("properties") {
            let range = match arg.as_str() {
                "target" => continue,
                "ip_version" => {
                    ip_versions += 1;
                    continue;
                }
                "count" => &COUNT_RANGE,
                "interval_ms" => &INTERVAL_MS_RANGE,
                "timeout_ms" => &TIMEOUT_MS_RANGE,
                "max_hops" => &MAX_HOPS_RANGE,
                "port" => &PORT_RANGE,
                other => panic!("{}: argument {other} has no limit constant", tool.name),
            };
            assert_eq!(
                schema["minimum"],
                json!(range.start()),
                "{}.{arg}",
                tool.name
            );
            assert_eq!(schema["maximum"], json!(range.end()), "{}.{arg}", tool.name);
            checked += 1;
        }
    }
    assert_eq!(checked, 3 + 4 + 2);
    assert_eq!(ip_versions, 3, "every tool takes ip_version");
}

/// The schema a model reads must list the three `ip_version` values.
#[tokio::test]
async fn input_schema_lists_ip_version_values() {
    let (client, _server) = connect(mock()).await;
    for tool in client.list_all_tools().await.unwrap() {
        let input = Value::Object((*tool.input_schema).clone());
        let text = input.to_string();
        for value in ["\"auto\"", "\"v4\"", "\"v6\""] {
            assert!(
                text.contains(value),
                "{}: {value} missing in {text}",
                tool.name
            );
        }
        assert!(
            input["required"]
                .as_array()
                .is_some_and(|r| r == &[json!("target")]),
            "{}: ip_version must be optional",
            tool.name
        );
    }
}

#[tokio::test]
async fn tool_descriptions_mention_real_traffic_and_duration() {
    let (client, _server) = connect(mock()).await;
    for tool in client.list_all_tools().await.unwrap() {
        let desc = tool.description.as_deref().unwrap_or_default();
        assert!(desc.contains("traffic"), "{}: {desc}", tool.name);
        assert!(desc.contains("60 s"), "{}: {desc}", tool.name);
        if tool.name == "traceroute" {
            assert!(desc.contains("root"), "{desc}");
        }
    }
}

#[tokio::test]
async fn tool_annotations_read_only_open_world_not_idempotent() {
    let (client, _server) = connect(mock()).await;
    for tool in client.list_all_tools().await.unwrap() {
        let a = tool.annotations.expect("annotations");
        assert_eq!(a.read_only_hint, Some(true), "{}", tool.name);
        assert_eq!(a.open_world_hint, Some(true), "{}", tool.name);
        assert_eq!(a.idempotent_hint, Some(false), "{}", tool.name);
    }
}

#[tokio::test]
async fn server_info_has_name_version_instructions_tools_only() {
    let (client, _server) = connect(mock()).await;
    let info = client.peer_info().expect("server info");
    let implementation = info.server_info.as_ref().expect("serverInfo");
    assert_eq!(implementation.name, "intqual");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    let instructions = info.instructions.as_deref().unwrap_or_default();
    assert!(instructions.contains("schema_version"), "{instructions}");
    assert!(instructions.contains("--mock"), "{instructions}");
    let caps = &info.capabilities;
    assert!(caps.tools.is_some());
    assert!(caps.resources.is_none());
    assert!(caps.prompts.is_none());
    assert!(caps.logging.is_none());
}

// ---------- results and errors ----------

#[tokio::test(start_paused = true)]
async fn ping_mock_returns_structured_report_with_icmp_only() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 3 }),
        ))
        .await
        .unwrap();
    let r = report(&result);
    assert_eq!(r.schema_version, 1);
    assert_eq!(r.source, Source::Mock);
    assert_eq!(r.count, 3);
    assert_eq!(r.icmp.expect("icmp").samples.len(), 3);
    assert!(r.tcp.is_none());
    assert!(r.traceroute.is_none());
    assert!(!r.interrupted);
}

#[tokio::test(start_paused = true)]
async fn tcp_latency_mock_returns_tcp_only_report() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "tcp_latency",
            json!({ "target": "example.test", "port": 8443, "count": 2 }),
        ))
        .await
        .unwrap();
    let r = report(&result);
    assert_eq!(r.port, 8443);
    assert_eq!(r.tcp.expect("tcp").samples.len(), 2);
    assert!(r.icmp.is_none());
    assert!(r.traceroute.is_none());
}

#[tokio::test(start_paused = true)]
async fn traceroute_mock_returns_three_hops_report() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call("traceroute", json!({ "target": "example.test" })))
        .await
        .unwrap();
    let r = report(&result);
    let trace = r.traceroute.expect("traceroute");
    assert!(trace.complete);
    assert_eq!(trace.hops.len(), 3);
    assert!(trace.error.is_none());
    assert!(r.icmp.is_none());
    assert!(r.tcp.is_none());
}

#[tokio::test(start_paused = true)]
async fn tool_result_text_block_equals_structured_content() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        ))
        .await
        .unwrap();
    assert_eq!(result.content.len(), 1);
    let text: Value =
        serde_json::from_str(&result.content[0].as_text().expect("text block").text).unwrap();
    assert_eq!(Some(text), result.structured_content);
}

#[tokio::test]
async fn invalid_count_returns_is_error_with_kind_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 0 }),
        ))
        .await
        .expect("a tool error is a result, not a protocol error");
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(e.message.contains("count"), "{}", e.message);
    assert!(result.structured_content.is_none());
}

#[tokio::test]
async fn over_budget_request_returns_is_error_not_protocol_error() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 20, "interval_ms": 5000, "timeout_ms": 5000 }),
        ))
        .await
        .expect("over budget is a tool error");
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(e.message.contains("60 s"), "{}", e.message);
}

#[tokio::test]
async fn over_budget_traceroute_returns_is_error_with_bound_in_text() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "traceroute",
            json!({ "target": "example.test", "max_hops": 30, "timeout_ms": 1000 }),
        ))
        .await
        .expect("over budget is a tool error");
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(
        e.message.contains("90000 ms") && e.message.contains("60 s"),
        "{}",
        e.message
    );
    // H4: traceroute has no count or interval, so the hint names its own arguments.
    assert!(
        e.message.contains("max_hops") && e.message.contains("timeout_ms"),
        "{}",
        e.message
    );
    assert!(!e.message.contains("count"), "{}", e.message);
}

/// H1/H2: the target is checked before anything runs, in every tool, with `--mock` too.
#[tokio::test]
async fn bad_targets_return_is_error_invalid_target() {
    let (client, _server) = connect(mock()).await;
    let cases = [
        ("", "target is empty"),
        ("   ", "target is empty"),
        ("https://x.com", "not a URL (try `x.com`)"),
        ("google.com; whoami", "contains a space"),
        ("google.com:443", "not `host:port`"),
    ];
    for tool in ["ping", "tcp_latency", "traceroute"] {
        for (target, hint) in cases {
            let result = client
                .call_tool(call(tool, json!({ "target": target })))
                .await
                .expect("a bad target is a tool error, not a protocol error");
            let e = tool_error(&result);
            assert_eq!(e.kind, "invalid_target", "{tool} {target:?}");
            assert!(e.message.contains(hint), "{tool} {target:?}: {}", e.message);
        }
    }
    let long = "a".repeat(300);
    let result = client
        .call_tool(call("ping", json!({ "target": long })))
        .await
        .unwrap();
    assert_eq!(tool_error(&result).kind, "invalid_target");
}

#[tokio::test]
async fn surrounding_whitespace_is_trimmed_in_the_report() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "  example.test  ", "count": 1 }),
        ))
        .await
        .unwrap();
    assert_eq!(report(&result).target, "example.test");
}

#[tokio::test]
async fn ip_version_v6_with_mock_reports_v6() {
    let (client, _server) = connect(mock()).await;
    for tool in ["ping", "tcp_latency"] {
        let result = client
            .call_tool(call(
                tool,
                json!({ "target": "example.test", "count": 1, "ip_version": "v6" }),
            ))
            .await
            .unwrap();
        let r = report(&result);
        assert_eq!(r.ip_version, intqual_core::IpFamily::V6, "{tool}");
        assert!(r.resolved_ip.contains(':'), "{tool}: {}", r.resolved_ip);
        let structured = result.structured_content.clone().unwrap();
        assert_eq!(structured["ip_version"], "v6");
    }
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        ))
        .await
        .unwrap();
    assert_eq!(report(&result).ip_version, intqual_core::IpFamily::V4);
}

#[tokio::test]
async fn ip_literal_of_the_other_family_is_no_address_for_family() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "127.0.0.1", "count": 1, "ip_version": "v6" }),
        ))
        .await
        .unwrap();
    let e = tool_error(&result);
    assert_eq!(e.kind, "no_address_for_family");
    assert!(e.message.contains("IPv6 was requested"), "{}", e.message);
}

#[tokio::test]
async fn unknown_ip_version_is_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call("ping", json!({ "target": "x", "ip_version": "ipv6" })))
        .await
        .unwrap();
    assert_eq!(tool_error(&result).kind, "invalid_request");
}

#[tokio::test]
async fn tcp_latency_port_zero_returns_is_error_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "tcp_latency",
            json!({ "target": "example.test", "port": 0 }),
        ))
        .await
        .unwrap();
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(e.message.contains("port"), "{}", e.message);
}

#[tokio::test]
async fn out_of_type_range_value_returns_message_naming_the_limit() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "traceroute",
            json!({ "target": "example.test", "max_hops": 300 }),
        ))
        .await
        .unwrap();
    let e = tool_error(&result);
    assert!(
        e.message.contains("max_hops must be between 1 and 64"),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn unknown_argument_returns_is_error_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "cout": 20 }),
        ))
        .await
        .unwrap();
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(e.message.contains("cout"), "{}", e.message);
}

#[tokio::test]
async fn dns_failure_returns_is_error_with_kind_dns_resolution() {
    let (client, _server) = connect(IntqualServer::new(Source::Network)).await;
    // `.invalid` never resolves (RFC 6761); the timeout keeps a slow resolver from hanging
    // the suite.
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        client.call_tool(call(
            "tcp_latency",
            json!({ "target": "nonexistent.invalid", "count": 1 }),
        )),
    )
    .await
    .expect("DNS lookup did not finish within 30 s")
    .unwrap();
    assert_eq!(tool_error(&result).kind, "dns_resolution");
}

#[tokio::test]
async fn unknown_tool_returns_protocol_error() {
    let (client, _server) = connect(mock()).await;
    let err = client
        .call_tool(call("speed_test", json!({ "target": "example.test" })))
        .await
        .expect_err("unknown tool is a protocol error");
    assert!(
        matches!(err, rmcp::ServiceError::McpError(_)),
        "got {err:?}"
    );
}

/// MCP 2025-11-25: input errors are tool execution errors, so the model can correct itself.
#[tokio::test]
async fn wrong_argument_type_returns_is_error_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": "x" }),
        ))
        .await
        .expect("bad arguments are a tool error, not a protocol error");
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    assert!(
        e.message.contains("invalid arguments") && e.message.contains("invalid type"),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn missing_target_returns_is_error_invalid_request() {
    let (client, _server) = connect(mock()).await;
    let result = client
        .call_tool(call("traceroute", json!({ "max_hops": 5 })))
        .await
        .expect("missing target is a tool error, not a protocol error");
    let e = tool_error(&result);
    assert_eq!(e.kind, "invalid_request");
    // H5: one prefix, not "Invalid request: invalid arguments: ...".
    assert_eq!(e.message, "invalid arguments: missing field `target`");
}

#[tokio::test]
async fn full_loss_ping_is_normal_result_not_error() {
    let server = IntqualServer::with_measurer(Source::Mock, AllTimeouts);
    let (client, _server) = connect(server).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 4 }),
        ))
        .await
        .unwrap();
    let r = report(&result);
    let icmp = r.icmp.expect("icmp");
    assert_eq!(icmp.summary.loss_pct, 100.0);
    assert_eq!(icmp.summary.received, 0);
}

// ---------- concurrency, cancellation, progress ----------

#[tokio::test(start_paused = true)]
async fn two_concurrent_calls_run_sequentially() {
    let (server, stats, _done) = recording();
    let (client, _server) = connect(server).await;
    let args = json!({ "target": "example.test", "count": 4, "interval_ms": 500 });
    let start = Instant::now();
    let (a, b) = tokio::join!(
        client.call_tool(call("ping", args.clone())),
        client.call_tool(call("tcp_latency", args)),
    );
    report(&a.unwrap());
    report(&b.unwrap());
    assert_eq!(stats.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        stats.max_active.load(Ordering::SeqCst),
        1,
        "calls overlapped"
    );
    // Each call takes about 4 × 500 ms; sequential means at least twice one call's sampling.
    assert!(
        start.elapsed() >= Duration::from_millis(2 * 1500),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_while_queued_releases_semaphore() {
    let (server, stats, _done) = recording();
    let (client, _server) = connect(server).await;
    let long = json!({ "target": "example.test", "count": 4, "interval_ms": 500 });
    let a = client.call_tool(call("ping", long));
    let b = client.send_cancellable_request(
        ClientRequest::CallToolRequest(Request::new(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        ))),
        PeerRequestOptions::no_options(),
    );
    let (a, b) = tokio::join!(a, async {
        let b = b.await.expect("send B");
        // A is measuring; B is queued behind it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        b.cancel(Some("test".into())).await.expect("cancel B");
    });
    let () = b;
    report(&a.unwrap());
    let c = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        )),
    )
    .await
    .expect("C must not wait for the cancelled B")
    .unwrap();
    report(&c);
    assert_eq!(
        stats.calls.load(Ordering::SeqCst),
        2,
        "B must never measure"
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_during_call_stops_measurement_with_interrupted_report() {
    let (server, _stats, mut done) = recording();
    let (client, _server) = connect(server).await;
    let handle = client
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(call(
                "ping",
                json!({ "target": "example.test", "count": 20, "interval_ms": 1000 }),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    handle.cancel(Some("test".into())).await.unwrap();
    // `measure` must stop within one interval; rmcp then drops the response.
    let interrupted = tokio::time::timeout(Duration::from_millis(1000), done.recv())
        .await
        .expect("measurement did not stop within one interval")
        .expect("recorder closed");
    assert!(
        interrupted,
        "a cancelled measurement reports interrupted: true"
    );
    // The server is still usable afterwards.
    let next = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        ))
        .await
        .unwrap();
    report(&next);
}

#[tokio::test(start_paused = true)]
async fn cancel_of_stalled_measurement_frees_queue_after_grace() {
    let server = IntqualServer::with_measurer(
        Source::Mock,
        StallOnce {
            calls: AtomicUsize::new(0),
        },
    );
    let (client, _server) = connect(server).await;
    let handle = client
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(call(
                "ping",
                json!({ "target": "example.test", "count": 1, "timeout_ms": 1000 }),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.cancel(None).await.unwrap();
    let start = Instant::now();
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        client.call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 1 }),
        )),
    )
    .await
    .expect("a stalled measurement must be abandoned after timeout + grace")
    .unwrap();
    report(&next);
    // Abandoned after timeout (1 s) + grace (1 s), then one 500 ms-interval sample.
    assert!(
        start.elapsed() >= Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn stdin_eof_cancels_running_call() {
    let (server, _stats, mut done) = recording();
    let (client, server_task) = connect(server).await;
    let peer = client.peer().clone();
    tokio::spawn(async move {
        let _ = peer
            .call_tool(call(
                "ping",
                json!({ "target": "example.test", "count": 20, "interval_ms": 1000 }),
            ))
            .await;
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Closing the client closes its end of the duplex: the server sees EOF.
    client.cancel().await.expect("client stops");
    let quit = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server must stop after EOF")
        .expect("server task panicked");
    assert!(quit.is_ok(), "waiting() failed: {quit:?}");
    let interrupted = tokio::time::timeout(Duration::from_secs(1), done.recv())
        .await
        .expect("running measurement must end")
        .expect("recorder closed");
    assert!(interrupted);
}

#[tokio::test(start_paused = true)]
async fn progress_sent_per_sample_when_token_present() {
    let (client, mut progress, _server) = connect_progress(mock()).await;
    let result = client
        .call_tool(call(
            "ping",
            json!({ "target": "example.test", "count": 4 }),
        ))
        .await
        .unwrap();
    report(&result);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut seen = Vec::new();
    while let Ok(p) = progress.try_recv() {
        assert_eq!(p.total, Some(4.0));
        seen.push(p.progress);
    }
    assert_eq!(seen, [1.0, 2.0, 3.0, 4.0]);
}

#[tokio::test(start_paused = true)]
async fn traceroute_progress_total_is_max_hops() {
    let (client, mut progress, _server) = connect_progress(mock()).await;
    let result = client
        .call_tool(call("traceroute", json!({ "target": "example.test" })))
        .await
        .unwrap();
    report(&result);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut seen = Vec::new();
    while let Ok(p) = progress.try_recv() {
        assert_eq!(p.total, Some(30.0));
        seen.push(p.progress);
    }
    // The mock path reaches the target at hop 3: no faked 100 %.
    assert_eq!(seen, [1.0, 2.0, 3.0]);
}

/// The rmcp client always attaches a progress token, so this test speaks raw JSON-RPC.
#[tokio::test(start_paused = true)]
async fn progress_not_sent_without_token() {
    let (s, c) = tokio::io::duplex(64 * 1024);
    let _server = spawn_server(mock(), s);
    let (read, mut write) = tokio::io::split(c);
    let mut lines = BufReader::new(read).lines();
    let messages = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "raw", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "ping", "arguments": { "target": "example.test", "count": 3 } } }),
    ];
    for m in messages {
        write.write_all(format!("{m}\n").as_bytes()).await.unwrap();
    }
    let mut progress = 0;
    loop {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("no response to tools/call")
            .unwrap()
            .expect("server closed the stream");
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["method"] == "notifications/progress" {
            progress += 1;
        }
        if v["id"] == 2 {
            assert_eq!(v["result"]["isError"], false, "{v}");
            break;
        }
    }
    assert_eq!(progress, 0);
}

// ---------- loopback (Source::Network, 127.0.0.1 only) ----------

#[tokio::test]
async fn tcp_latency_loopback_listener_succeeds() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let (client, _server) = connect(IntqualServer::new(Source::Network)).await;
    let result = client
        .call_tool(call(
            "tcp_latency",
            json!({ "target": "127.0.0.1", "port": port, "count": 3, "interval_ms": 100 }),
        ))
        .await
        .unwrap();
    let r = report(&result);
    assert_eq!(r.source, Source::Network);
    assert_eq!(r.tcp.expect("tcp").summary.received, 3);
}

#[tokio::test]
async fn tcp_latency_closed_port_reports_socket_loss_not_error() {
    // Bind, read the port, close: nothing listens there now.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let (client, _server) = connect(IntqualServer::new(Source::Network)).await;
    // Windows retries a refused loopback SYN for about 2 s before failing, so the timeout is
    // long enough for the refusal to arrive as `socket` rather than `tcp_timeout`.
    let result = client
        .call_tool(call(
            "tcp_latency",
            json!({ "target": "127.0.0.1", "port": port, "count": 1, "interval_ms": 100,
                    "timeout_ms": 5000 }),
        ))
        .await
        .unwrap();
    let r = report(&result);
    let tcp = r.tcp.expect("tcp");
    assert_eq!(tcp.summary.loss_pct, 100.0);
    let kind = &tcp.samples[0].error.as_ref().expect("error").kind;
    assert_eq!(kind, "socket");
}
