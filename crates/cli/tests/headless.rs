//! Runs the built `intqual` binary in headless mode. `--mock` keeps it off the network; the
//! DNS-failure cases use `.invalid` (RFC 6761), which never resolves, online or offline.

use std::process::{Command, Output};

fn intqual(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_intqual"))
        .args(args)
        // The binary writes `intqual.log` to its working directory; keep it out of the repo.
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .output()
        .expect("failed to run the intqual binary")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(o)))
}

#[test]
fn cli_json_mock_prints_valid_json_exit_0() {
    let o = intqual(&["--count", "3", "--json", "--mock", "--interval", "10"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v = json(&o);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["source"], "mock");
    assert_eq!(v["count"], 3);
    assert_eq!(v["icmp"]["samples"].as_array().map(Vec::len), Some(3));
    assert_eq!(v["tcp"]["samples"].as_array().map(Vec::len), Some(3));
    assert_eq!(v["traceroute"], serde_json::Value::Null);
}

#[test]
fn cli_json_mock_with_traceroute_includes_hops() {
    let o = intqual(&[
        "--count",
        "2",
        "--json",
        "--mock",
        "--interval",
        "10",
        "--traceroute",
    ]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(v["traceroute"]["complete"], true);
    assert_eq!(v["traceroute"]["hops"].as_array().map(Vec::len), Some(3));
}

#[test]
fn cli_json_without_count_exits_2() {
    for flag in ["--json", "--traceroute", "--no-icmp"] {
        let o = intqual(&[flag, "--mock"]);
        assert_eq!(o.status.code(), Some(2), "{flag} without --count");
        assert!(o.stdout.is_empty(), "{flag}: stdout should be empty");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("--count"),
            "{flag}: clap usage error expected, got {err}"
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&o.stderr).is_err(),
            "clap errors stay plain text"
        );
    }
}

#[test]
fn cli_invalid_count_exits_2_with_json_error() {
    let o = intqual(&["--count", "0", "--json", "--mock"]);
    assert_eq!(o.status.code(), Some(2));
    let v = json(&o);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["target"], "google.com");
    assert_eq!(v["error"]["kind"], "invalid_request");
}

#[test]
fn cli_dns_failure_json_exits_2_with_error_object() {
    let o = intqual(&["nonexistent.invalid", "--count", "1", "--json"]);
    assert_eq!(o.status.code(), Some(2));
    let v = json(&o);
    assert_eq!(v["target"], "nonexistent.invalid");
    assert_eq!(v["error"]["kind"], "dns_resolution");
}

#[test]
fn cli_dns_failure_text_writes_stderr_only() {
    let o = intqual(&["nonexistent.invalid", "--count", "1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty(), "stdout: {}", stdout(&o));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with("intqual: "), "stderr: {err}");
    assert!(err.contains("DNS Resolution Failed"), "stderr: {err}");
}

#[test]
fn cli_text_mode_mock_prints_summary_lines() {
    let o = intqual(&["--count", "3", "--mock", "--interval", "10"]);
    assert_eq!(o.status.code(), Some(0));
    let out = stdout(&o);
    assert!(out.starts_with("intqual google.com (192.0.2.1), port 443, 3 samples\n"));
    assert!(out.contains("ICMP  sent 3"), "got: {out}");
    assert!(out.contains("TCP   sent 3"), "got: {out}");
}

#[test]
fn cli_headless_does_not_enter_alternate_screen() {
    for args in [
        &["--count", "2", "--mock", "--interval", "10"][..],
        &["--count", "2", "--mock", "--interval", "10", "--json"][..],
    ] {
        let o = intqual(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}");
        assert!(!o.stdout.is_empty(), "{args:?}: the result goes to stdout");
        assert!(!stdout(&o).contains("\x1b[?1049h"), "{args:?}");
        assert!(
            !stdout(&o).contains('\x1b'),
            "{args:?}: no escape codes at all"
        );
    }
}

#[test]
fn cli_no_icmp_reports_null_icmp() {
    let o = intqual(&[
        "--count",
        "2",
        "--json",
        "--mock",
        "--interval",
        "10",
        "--no-icmp",
    ]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(v["icmp"], serde_json::Value::Null);
    assert_eq!(v["tcp"]["samples"].as_array().map(Vec::len), Some(2));
}

/// Real TCP to a closed port on 127.0.0.1 only (no internet, no admin). ICMP is off because
/// loopback ICMP would succeed and turn the exit code into 0.
#[test]
fn cli_closed_port_exits_1() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    // Windows retries a refused loopback SYN for about 2 s; 5 s keeps it a `socket` error.
    let o = intqual(&[
        "127.0.0.1",
        "--port",
        &port,
        "--count",
        "1",
        "--timeout",
        "5000",
        "--no-icmp",
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(1), "stdout: {}", stdout(&o));
    let v = json(&o);
    assert_eq!(v["icmp"], serde_json::Value::Null);
    assert_eq!(v["tcp"]["summary"]["received"], 0);
    assert_eq!(v["tcp"]["samples"][0]["error"]["kind"], "socket");
}
