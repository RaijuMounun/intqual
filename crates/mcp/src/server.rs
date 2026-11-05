//! The MCP server: three tools over `intqual_core::measure`.
//!
//! What rmcp 3.8 does that this module relies on (read from its source, `service.rs`):
//! - Each request runs in its own spawned task. `notifications/cancelled` and transport EOF
//!   cancel `RequestContext::ct`, but the handler future is **not** dropped; rmcp only drops
//!   the response of a cancelled request. So the handler must stop on its own: it passes a
//!   child of `ct` to `measure` and also bounds the wait after a cancel (`CANCEL_GRACE`).
//! - On stdin EOF rmcp waits up to 5 s for in-flight responses, then cancels running handlers.

use crate::args::{self, PingArgs, TcpArgs, TracerouteArgs};
use intqual_core::measure::{ErrorInfo, MeasureReport, MeasureRequest, Progress, Source};
use intqual_core::models::ProbeError;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    CallToolResult, ContentBlock, JsonObject, ProgressNotificationParam, ProgressToken,
};
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Error kind of a call that was cancelled before it produced a report (while queued, or a
/// measurement that did not stop in time). rmcp drops responses to cancelled requests, so a
/// client normally never sees it; it exists so the handler always has something to return.
pub const CANCELLED_KIND: &str = "cancelled";
/// Error kind for a failure inside the server itself (a report that would not serialize).
pub const INTERNAL_KIND: &str = "internal";

/// How long a cancelled measurement gets, on top of one sample timeout, to return its partial
/// report before the handler gives up on it. `measure` stops within one timeout.
const CANCEL_GRACE: Duration = Duration::from_secs(1);
/// Progress updates waiting to be forwarded. `measure` uses `try_send`, so a full channel
/// only drops updates; it never slows the measurement.
const PROGRESS_CAPACITY: usize = 32;

/// Runs one measurement. The server is generic over it (static dispatch), so tests can use a
/// source that stalls, fails or records what it was asked.
pub trait Measurer: Send + Sync + 'static {
    fn measure(
        &self,
        req: MeasureRequest,
        cancel: CancellationToken,
        progress: Option<mpsc::Sender<Progress>>,
    ) -> impl Future<Output = Result<MeasureReport, ProbeError>> + Send;
}

/// The real `intqual_core::measure::measure`. `req.source` picks network or mock data.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreMeasurer;

impl Measurer for CoreMeasurer {
    async fn measure(
        &self,
        req: MeasureRequest,
        cancel: CancellationToken,
        progress: Option<mpsc::Sender<Progress>>,
    ) -> Result<MeasureReport, ProbeError> {
        intqual_core::measure::measure(&req, cancel, progress).await
    }
}

/// The intqual MCP server. Serve it on any rmcp transport with `rmcp::ServiceExt::serve`.
#[derive(Clone)]
pub struct IntqualServer<M: Measurer = CoreMeasurer> {
    source: Source,
    measurer: Arc<M>,
    /// One measurement at a time: parallel calls queue here. Overlapping ICMP streams would
    /// distort each other's RTTs, and it keeps an agent from flooding the network.
    gate: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
}

impl IntqualServer<CoreMeasurer> {
    /// A server that measures with `intqual_core::measure` from `source`.
    pub fn new(source: Source) -> Self {
        Self::with_measurer(source, CoreMeasurer)
    }
}

#[tool_router]
impl<M: Measurer> IntqualServer<M> {
    /// A server over a custom measurement function (tests).
    pub fn with_measurer(source: Source, measurer: M) -> Self {
        Self {
            source,
            measurer: Arc::new(measurer),
            gate: Arc::new(Semaphore::new(1)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "ping",
        description = "Measure ICMP echo (ping) round-trip time, jitter and packet loss to a host. \
            Sends real ICMP traffic to the target unless the server runs with --mock. Takes about \
            count × interval_ms (2 s with the defaults), at most 60 s. Returns the intqual JSON \
            report (schema_version 1); 100% loss is a normal result, not an error.",
        input_schema = input_schema::<PingArgs>(),
        output_schema = rmcp::handler::server::tool::schema_for_output::<MeasureReport>(),
        annotations(read_only_hint = true, open_world_hint = true, idempotent_hint = false)
    )]
    async fn ping(&self, args: JsonObject, ctx: RequestContext<RoleServer>) -> CallToolResult {
        let request =
            args::parse::<PingArgs>(args).and_then(|a| args::ping_request(&a, self.source));
        self.run("ping", request, ctx).await
    }

    #[tool(
        name = "tcp_latency",
        description = "Measure TCP connect (handshake) time and failures to a host and port \
            (default 443). Sends real TCP traffic (full connects) to the target unless the server runs with \
            --mock. Takes about count × interval_ms (2 s with the defaults), at most 60 s. \
            Returns the intqual JSON report (schema_version 1).",
        input_schema = input_schema::<TcpArgs>(),
        output_schema = rmcp::handler::server::tool::schema_for_output::<MeasureReport>(),
        annotations(read_only_hint = true, open_world_hint = true, idempotent_hint = false)
    )]
    async fn tcp_latency(
        &self,
        args: JsonObject,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let request = args::parse::<TcpArgs>(args).and_then(|a| args::tcp_request(&a, self.source));
        self.run("tcp_latency", request, ctx).await
    }

    #[tool(
        name = "traceroute",
        description = "Trace the network path (hops, per-hop RTT, reverse-DNS names) to a host. \
            Sends real ICMP traffic with rising TTLs unless the server runs with --mock. Can take \
            up to max_hops × 3 × timeout_ms (45 s with the defaults), at most 60 s; it ends early \
            when the target replies. No admin rights needed on Windows; on Linux/macOS it may \
            need root and then reports error kind permission_denied. Returns the intqual JSON \
            report (schema_version 1).",
        input_schema = input_schema::<TracerouteArgs>(),
        output_schema = rmcp::handler::server::tool::schema_for_output::<MeasureReport>(),
        annotations(read_only_hint = true, open_world_hint = true, idempotent_hint = false)
    )]
    async fn traceroute(
        &self,
        args: JsonObject,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let request = args::parse::<TracerouteArgs>(args)
            .and_then(|a| args::traceroute_request(&a, self.source));
        self.run("traceroute", request, ctx).await
    }

    /// Shared body of the three tools: queue, measure with progress and cancel, build the
    /// result. Request errors become `isError` results so the model can fix its arguments.
    async fn run(
        &self,
        tool: &str,
        request: Result<MeasureRequest, ProbeError>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let req = match request {
            Ok(req) => req,
            Err(e) => {
                tracing::warn!("Rejected {} call: {}", tool, e);
                return error_result(&ErrorInfo::from(&e));
            }
        };
        let cancel = ctx.ct.child_token();

        let _permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::warn!("{} call cancelled while waiting for another measurement", tool);
                return error_result(&ErrorInfo {
                    kind: CANCELLED_KIND.to_string(),
                    message: "Cancelled while waiting for another measurement".to_string(),
                });
            }
            permit = self.gate.acquire() => match permit {
                Ok(permit) => permit,
                Err(e) => {
                    // The semaphore is never closed; this only guards against a future change.
                    tracing::error!("Measurement queue closed: {}", e);
                    return error_result(&ErrorInfo {
                        kind: INTERNAL_KIND.to_string(),
                        message: format!("Measurement queue closed: {e}"),
                    });
                }
            },
        };

        let token = ctx.meta.get_progress_token();
        let (progress_tx, progress_rx) = match token {
            Some(_) => {
                let (tx, rx) = mpsc::channel(PROGRESS_CAPACITY);
                (Some(tx), Some(rx))
            }
            None => (None, None),
        };
        // The measurement owns the only sender, so the forwarder ends once it returns, and
        // every notification is sent before the response.
        let (result, ()) = tokio::join!(
            measure_bounded(&*self.measurer, req, cancel, progress_tx),
            forward_progress(ctx.peer.clone(), token, progress_rx),
        );

        match result {
            Ok(report) => report_result(&report),
            Err(info) => {
                tracing::warn!("{} call failed: {}: {}", tool, info.kind, info.message);
                error_result(&info)
            }
        }
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "intqual",
    instructions = "intqual measures network quality from this machine. Use `ping` for ICMP \
        round-trip time, jitter and loss, `tcp_latency` for TCP connect time to a port (useful \
        when ICMP is blocked), and `traceroute` for the path to a host. Every tool returns a JSON \
        report with `schema_version`; probes not requested are null. Traffic is real unless the \
        server was started with --mock (then `source` is \"mock\"). Calls run one at a time and \
        each is limited to 60 s."
)]
impl<M: Measurer> ServerHandler for IntqualServer<M> {}

/// The tools take raw arguments and deserialize them in `args::parse`, so a type mismatch or a
/// missing `target` becomes an `invalid_request` tool error (MCP 2025-11-25: input errors are
/// tool errors, so the model can correct itself). The advertised schema is still the args type.
fn input_schema<T: schemars::JsonSchema + std::any::Any>() -> Arc<JsonObject> {
    rmcp::handler::server::common::schema_for_input::<T>().unwrap_or_else(|e| {
        // Only fails for a non-object root; every args type is a struct.
        tracing::error!(
            "Invalid input schema for {}: {}",
            std::any::type_name::<T>(),
            e
        );
        rmcp::handler::server::common::schema_for_type::<T>()
    })
}

/// Runs `measure` and makes sure the call ends soon after a cancel, even if the measurement
/// ignores the token (`CANCEL_GRACE` past one sample timeout).
async fn measure_bounded<M: Measurer>(
    measurer: &M,
    req: MeasureRequest,
    cancel: CancellationToken,
    progress: Option<mpsc::Sender<Progress>>,
) -> Result<MeasureReport, ErrorInfo> {
    let grace = req.timeout + CANCEL_GRACE;
    let watchdog = cancel.clone();
    tokio::select! {
        result = measurer.measure(req, cancel, progress) => result.map_err(|e| ErrorInfo::from(&e)),
        () = async {
            watchdog.cancelled().await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::error!("Measurement did not stop within {:?} of a cancel; abandoning it", grace);
            Err(ErrorInfo {
                kind: CANCELLED_KIND.to_string(),
                message: "Cancelled; the measurement did not stop in time".to_string(),
            })
        }
    }
}

/// Forwards `measure` progress as `notifications/progress` while the call has a token.
async fn forward_progress(
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    rx: Option<mpsc::Receiver<Progress>>,
) {
    let (Some(token), Some(mut rx)) = (token, rx) else {
        return;
    };
    while let Some(p) = rx.recv().await {
        let param = ProgressNotificationParam::new(token.clone(), f64::from(p.done))
            .with_total(f64::from(p.total));
        if let Err(e) = peer.notify_progress(param).await {
            tracing::warn!("Could not send a progress notification: {}", e);
        }
    }
}

/// A normal result: the report as structured content plus the same JSON as a text block, for
/// clients that ignore structured content.
fn report_result(report: &MeasureReport) -> CallToolResult {
    match serde_json::to_value(report) {
        Ok(value) => CallToolResult::structured(value),
        Err(e) => {
            tracing::error!("Could not serialize the report: {}", e);
            error_result(&ErrorInfo {
                kind: INTERNAL_KIND.to_string(),
                message: format!("Could not serialize the report: {e}"),
            })
        }
    }
}

/// A tool execution error: `isError: true` and one text block `{"kind": ..., "message": ...}`,
/// the same shape as errors inside the report.
fn error_result(info: &ErrorInfo) -> CallToolResult {
    let text = serde_json::to_string(info).unwrap_or_else(|e| {
        tracing::error!("Could not serialize a tool error: {}", e);
        format!("{}: {}", info.kind, info.message)
    });
    CallToolResult::error(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_result_is_one_text_block_with_kind_and_message() {
        let r = error_result(&ErrorInfo::from(&ProbeError::InvalidRequest("x".into())));
        assert_eq!(r.is_error, Some(true));
        assert!(r.structured_content.is_none());
        assert_eq!(r.content.len(), 1);
        let text = &r.content[0].as_text().unwrap().text;
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["kind"], "invalid_request");
        assert_eq!(v["message"], "x");
    }
}
