//! One-shot measurement: N ICMP and N TCP samples side by side, plus an optional traceroute,
//! returned as a `MeasureReport`. Used by the headless CLI (`--count`) and the MCP server.
//!
//! It runs the existing probes directly instead of going through `CoreEngine`: there is no
//! command loop to drive, so the engine would only add state.

pub mod report;
pub mod summary;

pub use report::{
    DROPPED_KIND, ErrorInfo, MeasureReport, ProbeReport, ReportHop, SCHEMA_VERSION, Sample, Source,
    Summary, TracerouteReport,
};
pub use summary::summarize;

use crate::engine::mock::{MOCK_HOPS, MOCK_IP, mock_hop};
use crate::models::ProbeError;
use crate::probe::{
    NetworkProbe, TelemetryEvent, ping::PingProbe, tcp::TcpProbe, traceroute::TracerouteProbe,
};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Upper bound on `MeasureRequest::count`.
pub const MAX_COUNT: u32 = 1000;
/// Default `max_hops`, the same as the TUI's traceroute.
pub const DEFAULT_MAX_HOPS: u8 = 30;

const MIN_INTERVAL: Duration = Duration::from_millis(10);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const MIN_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HOPS_LIMIT: u8 = 64;
/// Same capacity as the TUI pipeline.
const CHANNEL_CAPACITY: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureRequest {
    pub target: String,
    /// TCP port.
    pub port: u16,
    /// Samples per probe, 1..=`MAX_COUNT`.
    pub count: u32,
    pub interval: Duration,
    /// Per sample (and per traceroute probe / reverse-DNS lookup).
    pub timeout: Duration,
    pub icmp: bool,
    pub tcp: bool,
    pub traceroute: bool,
    pub max_hops: u8,
    pub source: Source,
}

impl MeasureRequest {
    /// Rejects requests outside the accepted bounds with `ProbeError::InvalidRequest`, so the
    /// CLI and the MCP server refuse the same inputs with the same message.
    pub fn validate(&self) -> Result<(), ProbeError> {
        let invalid = |msg: String| Err(ProbeError::InvalidRequest(msg));
        if !(1..=MAX_COUNT).contains(&self.count) {
            return invalid(format!(
                "count must be between 1 and {MAX_COUNT}, got {}",
                self.count
            ));
        }
        if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&self.interval) {
            return invalid(format!(
                "interval must be between {} ms and {} ms, got {} ms",
                MIN_INTERVAL.as_millis(),
                MAX_INTERVAL.as_millis(),
                self.interval.as_millis()
            ));
        }
        if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&self.timeout) {
            return invalid(format!(
                "timeout must be between {} ms and {} ms, got {} ms",
                MIN_TIMEOUT.as_millis(),
                MAX_TIMEOUT.as_millis(),
                self.timeout.as_millis()
            ));
        }
        if !(1..=MAX_HOPS_LIMIT).contains(&self.max_hops) {
            return invalid(format!(
                "max_hops must be between 1 and {MAX_HOPS_LIMIT}, got {}",
                self.max_hops
            ));
        }
        if self.tcp && self.port == 0 {
            return invalid("port must be between 1 and 65535 when TCP is on".to_string());
        }
        if !(self.icmp || self.tcp || self.traceroute) {
            return invalid("at least one of icmp, tcp or traceroute must be on".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Latency,
    Traceroute,
}

/// Progress of a running `measure`: samples per probe so far (`Latency`, `total = count`) or
/// hops so far (`Traceroute`, `total = max_hops`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub done: u32,
    pub total: u32,
}

/// Runs the measurement described by `req`.
///
/// `Err` only when nothing could be measured (invalid request, DNS failure, clock error);
/// every per-sample or traceroute failure is data in the report. `cancel` returns what has been
/// collected so far with `interrupted: true`. `progress` gets a best-effort `try_send` after
/// each latency tick and each hop, so a slow consumer never slows the measurement.
pub async fn measure(
    req: &MeasureRequest,
    cancel: CancellationToken,
    progress: Option<mpsc::Sender<Progress>>,
) -> Result<MeasureReport, ProbeError> {
    let clock: Clock = crate::utils::current_timestamp;
    match req.source {
        Source::Network => measure_with(req, cancel, progress, &NetworkSources, clock).await,
        Source::Mock => measure_with(req, cancel, progress, &MockSources { clock }, clock).await,
    }
}

/// UNIX seconds. A function pointer, so tests can pin it and golden JSON stays stable.
pub(crate) type Clock = fn() -> Result<u64, ProbeError>;

/// Where `measure` gets its data. Static dispatch: `NetworkSources` for real traffic,
/// `MockSources` for `Source::Mock`, and test doubles that stall or fail on purpose.
pub(crate) trait Sources: Sync {
    fn resolve(
        &self,
        req: &MeasureRequest,
    ) -> impl Future<Output = Result<SocketAddr, ProbeError>> + Send;

    /// Starts the requested latency probes. They send `Ping`/`Tcp` events on `tx` until
    /// `cancel` fires or `tx` closes.
    fn start_latency(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    );

    /// A traceroute that sends `TracerouteHop` events and a final `TracerouteComplete`.
    fn traceroute(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send + 'static;

    fn reverse_dns(&self, ip: IpAddr) -> impl Future<Output = Option<String>> + Send;
}

pub(crate) async fn measure_with<S: Sources>(
    req: &MeasureRequest,
    cancel: CancellationToken,
    progress: Option<mpsc::Sender<Progress>>,
    sources: &S,
    clock: Clock,
) -> Result<MeasureReport, ProbeError> {
    if let Err(e) = req.validate() {
        tracing::warn!("Rejected measure request: {}", e);
        return Err(e);
    }
    let started_at = match clock() {
        Ok(ts) => ts,
        Err(e) => {
            tracing::error!("Measure could not read the clock: {}", e);
            return Err(e);
        }
    };
    let start = Instant::now();
    // `resolve` logs its own failures.
    let addr = sources.resolve(req).await?;
    let progress = progress.as_ref();
    let mut interrupted = false;

    let (icmp, tcp) = if req.icmp || req.tcp {
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let probes = cancel.child_token();
        sources.start_latency(addr, req, tx, probes.clone());
        let outcome = collect_latency(&mut rx, req, &cancel, progress, started_at).await;
        probes.cancel();
        interrupted |= outcome.interrupted;
        (outcome.icmp, outcome.tcp)
    } else {
        (None, None)
    };

    // After ping/TCP, never alongside them, so the two ICMP streams do not mix.
    let traceroute = if !req.traceroute {
        None
    } else if cancel.is_cancelled() {
        interrupted = true;
        Some(TracerouteReport {
            complete: false,
            error: None,
            hops: Vec::new(),
        })
    } else {
        let (report, cancelled) = collect_traceroute(sources, addr, req, &cancel, progress).await;
        interrupted |= cancelled;
        Some(report)
    };

    Ok(MeasureReport {
        schema_version: SCHEMA_VERSION,
        target: req.target.clone(),
        resolved_ip: addr.ip().to_string(),
        port: req.port,
        count: req.count,
        interval_ms: req.interval.as_millis() as u64,
        timeout_ms: req.timeout.as_millis() as u64,
        source: req.source,
        started_at,
        duration_ms: start.elapsed().as_millis() as u64,
        interrupted,
        icmp,
        tcp,
        traceroute,
    })
}

/// Best effort: a full or closed progress channel never slows the measurement.
fn report_progress(progress: Option<&mpsc::Sender<Progress>>, phase: Phase, done: u32, total: u32) {
    if let Some(tx) = progress
        && let Err(mpsc::error::TrySendError::Full(_)) =
            tx.try_send(Progress { phase, done, total })
    {
        tracing::debug!("Progress channel full, dropping update");
    }
}

struct LatencyOutcome {
    icmp: Option<ProbeReport>,
    tcp: Option<ProbeReport>,
    interrupted: bool,
}

/// One probe's samples, indexed by `seq - 1`.
struct Slots {
    on: bool,
    samples: Vec<Option<Sample>>,
    filled: u32,
}

impl Slots {
    fn new(on: bool, count: u32) -> Self {
        Self {
            on,
            samples: vec![None; if on { count as usize } else { 0 }],
            filled: 0,
        }
    }

    fn insert(&mut self, seq: u64, timestamp: u64, result: &Result<f64, ProbeError>) {
        if !self.on || seq == 0 {
            return;
        }
        let Some(slot) = self.samples.get_mut((seq - 1) as usize) else {
            return; // past `count`: the probe ticked once more before it was cancelled
        };
        if slot.is_none() {
            *slot = Some(Sample::from_result(seq as u32, timestamp, result));
            self.filled += 1;
        }
    }

    /// `None` when the probe is off. Cancelled: only the samples taken. Otherwise every
    /// missing `seq` becomes `dropped`, so `sent == count`.
    fn finish(self, cancelled: bool, started_at: u64) -> Option<ProbeReport> {
        if !self.on {
            return None;
        }
        let samples: Vec<Sample> = self
            .samples
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Some(s) => Some(s),
                None if cancelled => None,
                None => Some(Sample::dropped(i as u32 + 1, started_at)),
            })
            .collect();
        Some(ProbeReport {
            summary: summarize(&samples),
            samples,
        })
    }
}

/// Collects `count` samples per enabled probe from `rx`, until done, cancelled, the sources
/// stop, or the overall deadline. Probes drop frames when the channel is full, so `count`
/// may never be reached; the deadline bounds the wait.
async fn collect_latency(
    rx: &mut mpsc::Receiver<TelemetryEvent>,
    req: &MeasureRequest,
    cancel: &CancellationToken,
    progress: Option<&mpsc::Sender<Progress>>,
    started_at: u64,
) -> LatencyOutcome {
    let mut icmp = Slots::new(req.icmp, req.count);
    let mut tcp = Slots::new(req.tcp, req.count);
    let deadline =
        Instant::now() + req.interval * req.count + req.timeout * 2 + Duration::from_secs(1);
    let done = |icmp: &Slots, tcp: &Slots| match (icmp.on, tcp.on) {
        (true, true) => icmp.filled.min(tcp.filled),
        (true, false) => icmp.filled,
        (false, true) => tcp.filled,
        (false, false) => req.count,
    };
    let mut reported = 0;
    let mut cancelled = false;

    while done(&icmp, &tcp) < req.count {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break;
            }
            event = rx.recv() => match event {
                Some(TelemetryEvent::Ping { sequence_number, result, timestamp, .. }) => {
                    icmp.insert(sequence_number, timestamp, &result);
                }
                Some(TelemetryEvent::Tcp { sequence_number, result, timestamp, .. }) => {
                    tcp.insert(sequence_number, timestamp, &result);
                }
                Some(TelemetryEvent::Fatal(e)) => {
                    tracing::error!("Latency probe reported a fatal error: {}", e);
                }
                Some(_) => {}
                None => {
                    tracing::warn!("Latency probes stopped before {} samples", req.count);
                    break;
                }
            },
            _ = tokio::time::sleep_until(deadline) => {
                tracing::warn!("Latency collection hit its deadline; missing samples are dropped");
                break;
            }
        }
        let now = done(&icmp, &tcp);
        if now > reported {
            reported = now;
            report_progress(progress, Phase::Latency, now, req.count);
        }
    }

    LatencyOutcome {
        icmp: icmp.finish(cancelled, started_at),
        tcp: tcp.finish(cancelled, started_at),
        interrupted: cancelled,
    }
}

/// Runs the traceroute source and collects its hops, each with a reverse-DNS hostname.
/// Returns the report and whether `cancel` fired; on cancel the hops so far are kept.
async fn collect_traceroute<S: Sources>(
    sources: &S,
    addr: SocketAddr,
    req: &MeasureRequest,
    cancel: &CancellationToken,
    progress: Option<&mpsc::Sender<Progress>>,
) -> (TracerouteReport, bool) {
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let probe_cancel = cancel.child_token();
    let task = tokio::spawn(sources.traceroute(addr, req, tx, probe_cancel.clone()));
    let mut hops: Vec<ReportHop> = Vec::new();
    let total = u32::from(req.max_hops);
    let interrupted = |hops: Vec<ReportHop>| {
        probe_cancel.cancel();
        (
            TracerouteReport {
                complete: false,
                error: None,
                hops,
            },
            true,
        )
    };

    loop {
        let event = tokio::select! {
            biased;
            _ = cancel.cancelled() => return interrupted(hops),
            event = rx.recv() => event,
        };
        match event {
            Some(TelemetryEvent::TracerouteHop(hop)) => {
                let ip: Option<IpAddr> = hop.ip_address.as_deref().and_then(|s| s.parse().ok());
                let mut report_hop = ReportHop::from_hop(hop, None);
                if let Some(ip) = ip {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            hops.push(report_hop);
                            return interrupted(hops);
                        }
                        name = tokio::time::timeout(req.timeout, sources.reverse_dns(ip)) => {
                            report_hop.hostname = name.unwrap_or_else(|_| {
                                tracing::warn!("Reverse DNS for {} timed out", ip);
                                None
                            });
                        }
                    }
                }
                hops.push(report_hop);
                report_progress(progress, Phase::Traceroute, hops.len() as u32, total);
            }
            Some(TelemetryEvent::TracerouteComplete) | None => break,
            Some(_) => {}
        }
    }

    let joined = tokio::select! {
        biased;
        _ = cancel.cancelled() => return interrupted(hops),
        joined = task => joined,
    };
    let error = match joined {
        Ok(Ok(())) => None,
        Ok(Err(e)) => {
            tracing::error!("Traceroute failed: {}", e);
            Some(ErrorInfo::from(&e))
        }
        Err(e) => {
            tracing::error!("Traceroute task panicked: {}", e);
            Some(ErrorInfo::from(&ProbeError::ThreadPanic(e.to_string())))
        }
    };
    let complete = hops.last().is_some_and(|h| h.is_destination);
    (
        TracerouteReport {
            complete,
            error,
            hops,
        },
        false,
    )
}

/// Real ICMP/TCP traffic through the existing probes.
pub(crate) struct NetworkSources;

impl Sources for NetworkSources {
    async fn resolve(&self, req: &MeasureRequest) -> Result<SocketAddr, ProbeError> {
        let addr_string = format!("{}:{}", req.target, req.port);
        match tokio::net::lookup_host(&addr_string).await {
            Ok(addrs) => crate::network::pick_address(addrs).ok_or_else(|| {
                tracing::error!("DNS resolution for {} returned no addresses", req.target);
                ProbeError::DnsResolution("No IP addresses found for host".to_string())
            }),
            Err(e) => {
                tracing::error!("DNS resolution for {} failed: {}", req.target, e);
                Err(ProbeError::DnsResolution(e.to_string()))
            }
        }
    }

    fn start_latency(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) {
        let target = Arc::new(req.target.clone());
        if req.icmp {
            let mut probe = PingProbe::new(
                target.clone(),
                addr,
                req.interval,
                req.timeout,
                crate::network::icmp::next_identifier(),
            );
            let (tx, cancel) = (tx.clone(), cancel.clone());
            tokio::spawn(async move {
                if let Err(e) = probe.run(tx, cancel).await {
                    tracing::error!("Headless ping probe stopped: {}", e);
                }
            });
        }
        if req.tcp {
            let mut probe = TcpProbe::new(target, addr, req.interval, req.timeout);
            tokio::spawn(async move {
                if let Err(e) = probe.run(tx, cancel).await {
                    tracing::error!("Headless TCP probe stopped: {}", e);
                }
            });
        }
    }

    fn traceroute(
        &self,
        addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send + 'static {
        let mut probe = TracerouteProbe::new(
            addr,
            req.timeout,
            req.max_hops,
            crate::network::icmp::next_identifier(),
        );
        async move { probe.run(tx, cancel).await }
    }

    async fn reverse_dns(&self, ip: IpAddr) -> Option<String> {
        match tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip)).await {
            Ok(Ok(name)) => Some(name),
            Ok(Err(e)) => {
                tracing::warn!("Reverse DNS for {} failed: {}", ip, e);
                None
            }
            Err(e) => {
                tracing::error!("Reverse DNS task for {} panicked: {}", ip, e);
                None
            }
        }
    }
}

/// `Source::Mock`: no traffic, no DNS. Samples come from `engine::mock::sample`, paced by
/// `interval` so timing behaves like the real path.
pub(crate) struct MockSources {
    pub(crate) clock: Clock,
}

impl Sources for MockSources {
    async fn resolve(&self, req: &MeasureRequest) -> Result<SocketAddr, ProbeError> {
        Ok(SocketAddr::new(IpAddr::V4(MOCK_IP), req.port))
    }

    fn start_latency(
        &self,
        _addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) {
        let clock = self.clock;
        let (interval, icmp, tcp) = (req.interval, req.icmp, req.tcp);
        let target = req.target.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            let mut seq: u64 = 0;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = timer.tick() => {}
                }
                seq += 1;
                let (icmp_result, tcp_result) = crate::engine::mock::sample(seq);
                let timestamp = clock().unwrap_or_else(|e| {
                    tracing::warn!("Mock clock failed: {}", e);
                    0
                });
                let mut events = Vec::with_capacity(2);
                if icmp {
                    events.push(TelemetryEvent::Ping {
                        sequence_number: seq,
                        target_ip: target.clone(),
                        result: icmp_result,
                        timestamp,
                    });
                }
                if tcp {
                    events.push(TelemetryEvent::Tcp {
                        sequence_number: seq,
                        target_ip: target.clone(),
                        result: tcp_result,
                        timestamp,
                    });
                }
                for event in events {
                    match tx.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::warn!("Mock telemetry channel full, dropping frame");
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            }
        });
    }

    fn traceroute(
        &self,
        _addr: SocketAddr,
        req: &MeasureRequest,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send + 'static {
        let max_hops = req.max_hops;
        async move {
            for (i, (ip, _, rtt)) in MOCK_HOPS.iter().enumerate().take(max_hops as usize) {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let hop = mock_hop(i, *ip, *rtt);
                if tx.send(TelemetryEvent::TracerouteHop(hop)).await.is_err() {
                    return Ok(());
                }
            }
            if tx.send(TelemetryEvent::TracerouteComplete).await.is_err() {
                tracing::warn!("Mock traceroute: collector gone before completion");
            }
            Ok(())
        }
    }

    async fn reverse_dns(&self, ip: IpAddr) -> Option<String> {
        MOCK_HOPS
            .iter()
            .find(|(hop_ip, _, _)| IpAddr::V4(*hop_ip) == ip)
            .map(|(_, name, _)| (*name).to_string())
    }
}

#[cfg(test)]
mod tests;
