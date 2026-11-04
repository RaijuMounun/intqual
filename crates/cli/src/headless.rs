//! Headless mode (`--count N [--json]`): one measurement, the result on stdout, an exit code a
//! script can check. Never touches crossterm/ratatui, so it works in pipes, CI and agents.

use intqual_core::measure::{
    ErrorInfo, MeasureReport, MeasureRequest, ProbeReport, SCHEMA_VERSION, measure,
};
use serde::Serialize;
use std::io::Write;
use tokio_util::sync::CancellationToken;

/// At least one ICMP or TCP sample succeeded.
pub const EXIT_OK: i32 = 0;
/// The measurement ran but no ICMP or TCP sample succeeded.
pub const EXIT_NO_REPLY: i32 = 1;
/// Nothing could be measured: invalid request, DNS failure, no address. Same code clap uses
/// for usage errors.
pub const EXIT_CANNOT_RUN: i32 = 2;

/// The stdout document for exit 2 under `--json`, so a caller that always parses stdout keeps
/// working. A struct (not `json!`) so the keys keep the spec's order, like the success document.
#[derive(Serialize)]
struct ErrorDocument<'a> {
    schema_version: u32,
    target: &'a str,
    error: ErrorInfo,
}

/// Exit code for a finished measurement. Traceroute results never change it.
pub fn exit_code(report: &MeasureReport) -> i32 {
    let any_reply = [&report.icmp, &report.tcp]
        .into_iter()
        .flatten()
        .any(|p| p.samples.iter().any(|s| s.rtt_ms.is_some()));
    if any_reply { EXIT_OK } else { EXIT_NO_REPLY }
}

/// The `ping`-style text summary. Per-sample lines are left to `--json`.
pub fn render_text(report: &MeasureReport) -> String {
    let mut text = format!(
        "intqual {} ({}), port {}, {} samples
",
        report.target, report.resolved_ip, report.port, report.count
    );
    if report.interrupted {
        text.push_str(
            "interrupted: partial results
",
        );
    }
    for (label, probe) in [("ICMP", &report.icmp), ("TCP", &report.tcp)] {
        if let Some(p) = probe {
            text.push_str(&summary_line(label, p));
        }
    }

    // Error kinds across both probes, in order of first appearance.
    let mut errors: Vec<(&str, u32)> = Vec::new();
    let all_samples = [&report.icmp, &report.tcp]
        .into_iter()
        .flatten()
        .flat_map(|p| p.samples.iter());
    for kind in all_samples.filter_map(|s| s.error.as_ref().map(|e| e.kind.as_str())) {
        match errors.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => errors.push((kind, 1)),
        }
    }
    if !errors.is_empty() {
        let list: Vec<String> = errors.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
        text.push_str(&format!(
            "errors: {}
",
            list.join(", ")
        ));
    }

    if let Some(trace) = &report.traceroute {
        let state = if trace.complete {
            "complete"
        } else {
            "incomplete"
        };
        text.push_str(&format!(
            "traceroute ({state})
"
        ));
        text.push_str(&format!(
            "{:<5}{:<17}{:<32}{}
",
            "#", "IP", "hostname", "RTT"
        ));
        for hop in &trace.hops {
            text.push_str(&format!(
                "{:<5}{:<17}{:<32}{}
",
                hop.hop_number,
                hop.ip_address.as_deref().unwrap_or("*"),
                hop.hostname.as_deref().unwrap_or("-"),
                hop.avg_rtt_ms
                    .map_or_else(|| "-".to_string(), |ms| format!("{ms:.1} ms")),
            ));
        }
        if let Some(e) = &trace.error {
            text.push_str(&format!(
                "traceroute error: {}: {}
",
                e.kind, e.message
            ));
        }
    }
    text
}

fn summary_line(label: &str, p: &ProbeReport) -> String {
    let s = &p.summary;
    let ms = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"));
    format!(
        "{label:<6}sent {:<4}received {:<4}loss {:<7}min/avg/max {}/{}/{} ms  jitter {} ms
",
        s.sent,
        s.received,
        format!("{:.1}%", s.loss_pct),
        ms(s.min_ms),
        ms(s.avg_ms),
        ms(s.max_ms),
        ms(s.jitter_ms),
    )
}

/// Runs one measurement and writes the result to `out` (JSON or text) and human error
/// messages to `err`. Returns the process exit code.
pub async fn run(
    req: &MeasureRequest,
    json: bool,
    mut out: impl Write,
    mut err: impl Write,
) -> i32 {
    // The CLI does not wire Ctrl+C in v1: the token is never cancelled.
    let (code, written) = match measure(req, CancellationToken::new(), None).await {
        Ok(report) => {
            let code = exit_code(&report);
            let written = if json {
                serde_json::to_string_pretty(&report)
                    .map_err(std::io::Error::other)
                    .and_then(|doc| writeln!(out, "{doc}"))
            } else {
                out.write_all(render_text(&report).as_bytes())
            };
            (code, written)
        }
        Err(e) => {
            // `measure` already logged the cause.
            let written = if json {
                let doc = ErrorDocument {
                    schema_version: SCHEMA_VERSION,
                    target: &req.target,
                    error: ErrorInfo::from(&e),
                };
                serde_json::to_string_pretty(&doc)
                    .map_err(std::io::Error::other)
                    .and_then(|doc| writeln!(out, "{doc}"))
            } else {
                writeln!(err, "intqual: {e}")
            };
            (EXIT_CANNOT_RUN, written)
        }
    };
    if let Err(e) = written.and_then(|()| out.flush()) {
        // stdout is gone (closed pipe): nobody can read the result, so do not claim success.
        tracing::error!("Failed to write the headless result: {}", e);
        let _ = writeln!(err, "intqual: failed to write the result: {e}");
        return code.max(EXIT_NO_REPLY);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use intqual_core::measure::{ReportHop, Sample, Source, TracerouteReport, summarize};
    use intqual_core::models::ProbeError;

    fn probe(results: &[Result<f64, ProbeError>]) -> ProbeReport {
        let samples: Vec<Sample> = results
            .iter()
            .enumerate()
            .map(|(i, r)| Sample::from_result(i as u32 + 1, 0, r))
            .collect();
        ProbeReport {
            summary: summarize(&samples),
            samples,
        }
    }

    fn report(icmp: Option<ProbeReport>, tcp: Option<ProbeReport>) -> MeasureReport {
        MeasureReport {
            schema_version: SCHEMA_VERSION,
            target: "example.test".to_string(),
            resolved_ip: "192.0.2.1".to_string(),
            port: 443,
            count: 3,
            interval_ms: 500,
            timeout_ms: 1000,
            source: Source::Mock,
            started_at: 0,
            duration_ms: 1000,
            interrupted: false,
            icmp,
            tcp,
            traceroute: None,
        }
    }

    fn all_lost() -> ProbeReport {
        probe(&[Err(ProbeError::TcpTimeout), Err(ProbeError::TcpTimeout)])
    }

    fn failed_trace() -> TracerouteReport {
        TracerouteReport {
            complete: false,
            error: Some(ErrorInfo::from(&ProbeError::PermissionDenied)),
            hops: vec![],
        }
    }

    #[test]
    fn exit_code_is_0_when_only_tcp_succeeds() {
        let ok = || probe(&[Ok(10.0)]);
        assert_eq!(exit_code(&report(Some(all_lost()), Some(ok()))), 0);
        assert_eq!(exit_code(&report(Some(ok()), Some(all_lost()))), 0);
        assert_eq!(exit_code(&report(Some(all_lost()), Some(all_lost()))), 1);
        // A probe that was off does not count either way.
        assert_eq!(exit_code(&report(None, Some(ok()))), 0);
        assert_eq!(exit_code(&report(Some(ok()), None)), 0);
        assert_eq!(exit_code(&report(None, Some(all_lost()))), 1);
        assert_eq!(exit_code(&report(None, None)), 1);
    }

    #[test]
    fn render_text_marks_interrupted_report() {
        let mut r = report(Some(probe(&[Ok(10.0)])), None);
        r.interrupted = true;
        let text = render_text(&r);
        let mut lines = text.lines();
        assert!(
            lines
                .next()
                .is_some_and(|l| l.starts_with("intqual example.test"))
        );
        assert_eq!(lines.next(), Some("interrupted: partial results"));
        assert!(!render_text(&report(Some(probe(&[Ok(10.0)])), None)).contains("interrupted"));
    }

    /// A stdout whose reader is gone (closed pipe).
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[tokio::test]
    async fn run_returns_nonzero_when_stdout_write_fails() {
        let req = MeasureRequest {
            count: 1,
            ..invalid_req()
        };
        for json in [true, false] {
            let mut err = Vec::new();
            let code = run(&req, json, ClosedPipe, &mut err).await;
            assert_eq!(
                code, EXIT_NO_REPLY,
                "json={json}: a lost result is not success"
            );
            let err = String::from_utf8_lossy(&err);
            assert!(err.contains("failed to write the result"), "stderr: {err}");
        }
    }

    #[test]
    fn exit_code_ignores_traceroute_result() {
        let mut ok = report(Some(probe(&[Ok(10.0)])), Some(all_lost()));
        ok.traceroute = Some(failed_trace());
        assert_eq!(exit_code(&ok), 0);

        let mut lost = report(Some(all_lost()), Some(all_lost()));
        lost.traceroute = Some(TracerouteReport {
            complete: true,
            error: None,
            hops: vec![],
        });
        assert_eq!(exit_code(&lost), 1);
    }

    #[test]
    fn render_text_prints_header_and_two_summary_lines() {
        let icmp = probe(&[Ok(11.0), Err(ProbeError::IcmpTimeout), Ok(13.0)]);
        let tcp = probe(&[Ok(14.0), Ok(15.0), Ok(17.0)]);
        let text = render_text(&report(Some(icmp), Some(tcp)));
        let expected = "\
intqual example.test (192.0.2.1), port 443, 3 samples
ICMP  sent 3   received 2   loss 33.3%  min/avg/max 11.0/12.0/13.0 ms  jitter 2.0 ms
TCP   sent 3   received 3   loss 0.0%   min/avg/max 14.0/15.3/17.0 ms  jitter 1.5 ms
errors: icmp_timeout ×1
";
        assert_eq!(text, expected);
    }

    #[test]
    fn render_text_prints_dashes_when_nothing_was_received() {
        let text = render_text(&report(None, Some(all_lost())));
        assert!(!text.contains("ICMP"));
        assert!(
            text.contains(
                "TCP   sent 2   received 0   loss 100.0% min/avg/max -/-/- ms  jitter - ms"
            )
        );
        assert!(text.contains("errors: tcp_timeout ×2"));
    }

    #[test]
    fn render_text_with_traceroute_prints_hop_table_and_error() {
        let mut r = report(Some(probe(&[Ok(10.0)])), Some(probe(&[Ok(12.0)])));
        r.traceroute = Some(TracerouteReport {
            complete: false,
            error: Some(ErrorInfo::from(&ProbeError::PermissionDenied)),
            hops: vec![
                ReportHop {
                    hop_number: 1,
                    ip_address: Some("192.168.1.1".into()),
                    hostname: Some("router.lan".into()),
                    avg_rtt_ms: Some(1.24),
                    is_destination: false,
                    icmp_error: None,
                },
                ReportHop {
                    hop_number: 2,
                    ip_address: None,
                    hostname: None,
                    avg_rtt_ms: None,
                    is_destination: false,
                    icmp_error: None,
                },
            ],
        });
        let text = render_text(&r);
        let table = "\
traceroute (incomplete)
#    IP               hostname                        RTT
1    192.168.1.1      router.lan                      1.2 ms
2    *                -                               -
traceroute error: permission_denied: Permission Denied / Unsupported
";
        assert!(text.ends_with(table), "got:\n{text}");
        assert!(!text.contains("errors:"), "no sample errors here");
    }

    fn invalid_req() -> MeasureRequest {
        MeasureRequest {
            target: "example.test".to_string(),
            port: 443,
            count: 0,
            interval: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(100),
            icmp: true,
            tcp: true,
            traceroute: false,
            max_hops: 30,
            source: Source::Mock,
        }
    }

    #[tokio::test]
    async fn run_json_success_and_error_are_both_pretty_printed() {
        let ok_req = MeasureRequest {
            count: 1,
            ..invalid_req()
        };
        for req in [ok_req, invalid_req()] {
            let mut out = Vec::new();
            run(&req, true, &mut out, std::io::sink()).await;
            let text = String::from_utf8(out).unwrap();
            assert!(
                text.starts_with("{\n  \"schema_version\": 1,\n"),
                "count={}: not pretty-printed: {text}",
                req.count
            );
            assert!(text.ends_with("}\n"));
            // Same key order as the spec: schema_version, target, then the rest.
            let target = text.find("\"target\"").unwrap();
            assert!(text.find("\"schema_version\"").unwrap() < target);
            let next = if req.count == 0 {
                "\"error\""
            } else {
                "\"resolved_ip\""
            };
            assert!(target < text.find(next).unwrap(), "key order: {text}");
        }
    }

    #[tokio::test]
    async fn run_writes_json_error_object_on_invalid_request() {
        let req = invalid_req();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&req, true, &mut out, &mut err).await;
        assert_eq!(code, EXIT_CANNOT_RUN);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["target"], "example.test");
        assert_eq!(v["error"]["kind"], "invalid_request");
        assert!(v["error"]["message"].is_string());
    }
}
