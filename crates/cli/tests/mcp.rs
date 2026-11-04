//! Runs the built `intqual` binary as an MCP server (`intqual mcp --mock`, no network traffic)
//! and checks the stdio contract: stdout carries only JSON-RPC, logs go to stderr, and the exit
//! code follows stdin.

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(20);

/// A fresh, empty working directory per test, so a stray `intqual.log` is detectable.
fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_mcp(dir: &PathBuf, rust_log: Option<&str>) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_intqual"));
    cmd.args(["mcp", "--mock"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match rust_log {
        Some(level) => cmd.env("RUST_LOG", level),
        None => cmd.env_remove("RUST_LOG"),
    };
    cmd.spawn().expect("failed to start intqual mcp")
}

/// Waits for the child on another thread, so a hung server fails the test instead of blocking.
/// On timeout the child is killed by pid, since the waiting thread owns the `Child`.
fn wait_with_deadline(child: Child) -> Output {
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(DEADLINE) {
        Ok(output) => output.expect("failed to wait for intqual mcp"),
        Err(_) => {
            kill(pid);
            panic!("intqual mcp did not exit before the deadline");
        }
    }
}

fn kill(pid: u32) {
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    if let Err(e) = status {
        eprintln!("could not kill intqual mcp (pid {pid}): {e}");
    }
}

/// `initialize`, `notifications/initialized`, `tools/list`, then stdin is closed.
fn session(rust_log: Option<&str>, dir: &PathBuf) -> Output {
    let mut child = spawn_mcp(dir, rust_log);
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "cli-test", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    ] {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    wait_with_deadline(child)
}

fn stdout_lines(o: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"))
        })
        .collect()
}

fn code(status: ExitStatus) -> Option<i32> {
    status.code()
}

#[test]
fn cli_mcp_mock_stdout_only_jsonrpc_lines() {
    let dir = temp_dir("stdout");
    let o = session(None, &dir);
    assert!(
        !dir.join("intqual.log").exists(),
        "default run wrote intqual.log"
    );
    let lines = stdout_lines(&o);
    assert!(!lines.is_empty(), "no output");
    for v in &lines {
        assert_eq!(v["jsonrpc"], "2.0", "not JSON-RPC 2.0: {v}");
    }
    let tools = lines
        .iter()
        .find(|v| v["id"] == 2)
        .expect("tools/list response");
    let mut names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["ping", "tcp_latency", "traceroute"]);
}

#[test]
fn cli_mcp_stdin_eof_exits_0() {
    let o = session(None, &temp_dir("eof"));
    assert_eq!(
        code(o.status),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn cli_mcp_logs_to_stderr_not_file() {
    let dir = temp_dir("logs");
    let o = session(Some("info"), &dir);
    assert_eq!(code(o.status), Some(0));
    assert!(
        !dir.join("intqual.log").exists(),
        "intqual mcp wrote intqual.log"
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("INFO"), "no log lines on stderr: {stderr}");
    // Every stdout line parsing as JSON-RPC proves no log line leaked there.
    for v in stdout_lines(&o) {
        assert_eq!(v["jsonrpc"], "2.0");
    }
}

/// Closing stdin before `initialize` is a start failure: exit 1, the reason on stderr.
#[test]
fn cli_mcp_start_failure_exits_1() {
    let mut child = spawn_mcp(&temp_dir("start-failure"), None);
    drop(child.stdin.take());
    let o = wait_with_deadline(child);
    assert_eq!(code(o.status), Some(1));
    assert!(
        o.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("failed to start"), "stderr: {stderr}");
}

fn headless(name: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_intqual"))
        .args(args)
        .current_dir(temp_dir(name))
        .output()
        .expect("failed to run intqual")
}

#[test]
fn cli_positional_target_still_works_after_subcommand() {
    let o = headless(
        "positional",
        &["--count", "1", "--json", "--mock", "--interval", "10", "x"],
    );
    assert_eq!(code(o.status), Some(0));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["target"], "x");
}

#[test]
fn cli_dash_dash_mcp_is_treated_as_host() {
    let o = headless(
        "dash-dash",
        &[
            "--count",
            "1",
            "--json",
            "--mock",
            "--interval",
            "10",
            "--",
            "mcp",
        ],
    );
    assert_eq!(code(o.status), Some(0));
    let v: Value = serde_json::from_slice(&o.stdout).expect("headless JSON, not the MCP server");
    assert_eq!(v["target"], "mcp");
    assert_eq!(v["schema_version"], 1);
}
