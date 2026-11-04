# Spec: Headless mode (`--count N [--json]`)

Status: **implemented** · Date: 2026-11-04 (revised after manual testing, see the end) · Scope: all platforms (verified on Windows only; Unix paths
compile in CI but are not run by hand).

## Problem

intqual only runs as a full-screen TUI. Scripts, CI jobs and AI agents cannot run it, cannot tell
whether it worked, and cannot read its numbers. The measurement logic is also tied to the UI:
loss %, jitter and min/max live in `NetworkStats` in `crates/cli/src/ui/mod.rs`, and probes only
talk to the TUI through `CoreEngine`'s command loop.

The planned MCP server (`docs/specs/mcp-server.md`) needs the same thing: one call that takes a
target and returns structured results.

## Goal

```
intqual google.com --count 10            # human-readable summary, then exit
intqual google.com --count 10 --json     # one JSON document on stdout, then exit
intqual google.com --count 5 --json --traceroute
intqual --count 5 --json --mock          # no network traffic, deterministic output
```

It takes N ICMP and N TCP samples side by side (plus an optional traceroute), prints the result
and exits with a code a script can check. Errors are data in the output, not crashes. Without
`--count`, intqual opens the TUI exactly as it does today.

## Approach

### 1. Shared measurement function in core (`crates/core/src/measure/`)

A new public module, which the headless CLI and the MCP server both call. No UI or terminal
dependencies.

```rust
pub struct MeasureRequest {
    pub target: String,
    pub port: u16,            // TCP port
    pub count: u32,           // samples per probe, 1..=MAX_COUNT
    pub interval: Duration,
    pub timeout: Duration,    // per sample
    pub icmp: bool,           // run the ICMP probe (CLI: always true)
    pub tcp: bool,            // run the TCP probe  (CLI: always true)
    pub traceroute: bool,
    pub max_hops: u8,         // default 30, same as the TUI
    pub source: Source,       // Source::Network | Source::Mock
}

pub async fn measure(
    req: &MeasureRequest,
    cancel: CancellationToken,
    progress: Option<mpsc::Sender<Progress>>,
) -> Result<MeasureReport, ProbeError>;

pub struct Progress { pub phase: Phase, pub done: u32, pub total: u32 } // Phase: Latency | Traceroute
```

- `progress` (added for the MCP server's progress notifications; the CLI passes `None`): after
  each latency tick (`done` = samples per probe so far, `total` = `count`) and after each hop
  (`done` = hops so far, `total` = `max_hops`), `measure` sends a `Progress` with `try_send`,
  dropping it when full, so a slow consumer never slows the measurement. When traceroute stops
  early at the destination, no final `done == total` is faked.

- `Err` only when no measurement could start: invalid request, DNS failure, no address, clock
  error. Everything else (timeouts, permission denied, refused ports, traceroute failure) is data
  in the report.
- `icmp` / `tcp` select which latency probes run, so the MCP tools `ping` and `tcp_latency` can
  run one each. A probe that is off is `null` in the report. At least one of `icmp`, `tcp`,
  `traceroute` must be on (`validate()`). The CLI always sets both latency probes on.
- Resolves the target once with `pick_address` (IPv4 preferred, same as the engine).
- Reuses the existing probes: runs `PingProbe` and `TcpProbe` on one `mpsc` channel with a
  child `CancellationToken`, collects events until each probe has `count` samples, then cancels.
  It does not go through `CoreEngine`: there is no command loop to drive, so it would only add
  state.
- **Overall deadline:** probes drop frames when the channel is full (`try_send`), so a sample can
  be lost and `count` never reached. Collection also stops at
  `count × interval + 2 × timeout + 1 s`. Missing samples are listed with their `seq`,
  `rtt_ms: null`, `error.kind: "dropped"` and `timestamp = started_at`, so `sent` always equals
  `count` and dropped samples count as loss. The call always returns within that bound (plus the
  traceroute bound below).
- **Traceroute** (when `req.traceroute`): runs after ping/TCP, not at the same time, so the two
  ICMP streams do not mix. Uses `TracerouteProbe` with `req.timeout` and `req.max_hops`. A
  failure (for example `PermissionDenied` on Unix without root) goes into `traceroute.error` and
  any hops already received are kept. It never spawns `sudo`. Each hop with an IP gets a
  reverse-DNS lookup (`dns-lookup` in `spawn_blocking`, bounded by `timeout`); a failed or slow
  lookup gives `hostname: null`. Bound: `max_hops × 3 × timeout` plus DNS.
- `cancel` stops everything; `measure` then returns `Ok` with what it has and
  `interrupted: true`. Samples not taken are omitted (not `dropped`), so `sent` is the number
  actually taken. If cancel arrives during traceroute, the hops received so far are kept with
  `complete: false`. (Used by MCP request cancellation. The CLI does not wire Ctrl+C in v1.)
- `Source::Mock` sends no traffic and does no DNS: any target resolves to `192.0.2.1`
  (TEST-NET-1). Samples come from `MockEngine::sample` (moved into a shared pure function);
  traceroute returns three fixed hops in `192.0.2.0/24`, the last one the destination, with fixed
  hostnames. It still waits `interval` between samples so timing behaves like the real path;
  tests use `tokio::time::pause()`.
- **Validation:** `MeasureRequest::validate()` accepts `count` 1..=1000 (`MAX_COUNT`),
  `interval` 10 ms..=60 s, `timeout` 100 ms..=30 s, `max_hops` 1..=64, `port` 1..=65535 (when
  `tcp` is on), and at least one probe on. "No address found" uses kind `dns_resolution`. Failure returns a new `ProbeError::InvalidRequest(String)` (kind `invalid_request`), so the
  CLI (exit 2) and MCP reject the same inputs with the same message. `measure` calls it first.
- **Testability:** the collection step is a private generic function over its event source
  (static dispatch), so tests can feed a stalling or failing source to hit the `dropped` and
  traceroute-failure paths without sockets. The clock (`started_at`, sample timestamps) comes
  through a parameter so golden JSON tests are stable.

### 2. Report types (`measure/report.rs`)

Plain structs with `#[derive(Serialize)]` (`serde` is already a core dependency). Their JSON
form is the output contract, versioned with `schema_version`.

```jsonc
{
  "schema_version": 1,
  "target": "google.com",
  "resolved_ip": "142.250.187.206",
  "port": 443,
  "count": 10, "interval_ms": 500, "timeout_ms": 1000,
  "source": "network",                       // or "mock"
  "started_at": 1762250000,                  // UNIX seconds
  "duration_ms": 5120,
  "interrupted": false,
  "icmp": {
    "summary": { "sent": 10, "received": 9, "loss_pct": 10.0,
                 "min_ms": 11.2, "avg_ms": 13.4, "max_ms": 18.9, "jitter_ms": 1.7 },
    "samples": [
      { "seq": 1, "timestamp": 1762250000, "rtt_ms": 12.1, "error": null },
      { "seq": 2, "timestamp": 1762250001, "rtt_ms": null,
        "error": { "kind": "icmp_timeout", "message": "ICMP Timeout" } }
    ]
  },
  "tcp": { "summary": { ... }, "samples": [ ... ] },
  "traceroute": null                         // or the object below when --traceroute
}
```

```jsonc
"traceroute": {
  "complete": true,                          // reached the destination
  "error": null,                             // or { "kind": ..., "message": ... }
  "hops": [ { "hop_number": 1, "ip_address": "192.168.1.1", "hostname": "router.lan",
              "avg_rtt_ms": 1.2, "is_destination": false, "icmp_error": null } ]
}
```

- **Errors:** `ProbeError` is not serializable and wraps `io::Error`, so the report stores
  `{ kind, message }`. A new `ProbeError::kind(&self) -> &'static str` returns a stable
  snake_case name per variant (`icmp_timeout`, `tcp_timeout`, `permission_denied`, `socket`,
  `dns_resolution`, …). `message` is the `Display` text. Plus `dropped` for lost frames (above).
  Scripts should match on `kind`, never on `message`.
- **Summary maths** is one pure function, `summarize(&[Sample]) -> Summary`:
  `received` = samples with `rtt_ms`; `loss_pct = (sent - received) / sent × 100`; every error
  counts as loss, including `socket` (refused port). This differs from the TUI's `NetworkStats`,
  which ignores non-timeout errors (assessment §5 item 9). `jitter_ms` = mean absolute
  difference between consecutive successful RTTs, same as the TUI. `min/avg/max/jitter` are
  `null` when `received == 0` (and jitter when `received < 2`).
- **Rounding:** every millisecond value in the report (`rtt_ms`, `min/avg/max/jitter_ms`, hop
  `avg_rtt_ms`) and `loss_pct` is rounded to 3 decimals (`round_ms`). Raw `f64` timings print as
  `82.27239999999999`. Summary maths runs on full precision and rounds once at the end.
- `TracerouteHop` (IPC contract) is not changed; the report has its own hop struct with
  `hostname` added.
- **`icmp_error` on hops:** `null` for Echo Reply (the destination) and for Time Exceeded in
  transit (type 11, code 0), which is how every intermediate router normally answers; reporting
  it made a healthy path look like one error per hop. It stays set for real problems:
  Destination Unreachable (`"Type: 3, Code: N"`), fragment-reassembly Time Exceeded (code 1)
  and unknown types. The rule lives in `classify_response` in `probe/traceroute.rs`, so the TUI
  and the Unix worker's JSON lines follow it too (same field, fewer non-null values).

### 3. CLI (`crates/cli/src/headless.rs`)

- New flags on the existing `Cli`: `--count <N>` (`-c`), `--json`, `--traceroute`.
  `--json` and `--traceroute` `requires = "count"`, so clap rejects them without it.
  `--port`, `--interval`, `--timeout`, `--mock` keep their meaning. `--count` turns on headless
  mode; `--mock` with `--count` means `Source::Mock`.
- `headless.rs` exposes pure `exit_code(&MeasureReport) -> i32` and
  `render_text(&MeasureReport) -> String`, and a `run(.., out: impl Write)` so most CLI tests
  need no subprocess.
- `main.rs` branches before creating the engine or the terminal: headless never touches
  crossterm/ratatui and never enters the alternate screen.
- **stdout carries only the result** (the JSON document or the text summary). Logs still go to
  `intqual.log`, as today. Human error messages go to stderr.
- **Text format** (default), modelled on `ping`:

  ```
  intqual google.com (142.250.187.206), port 443, 10 samples
  ICMP  sent 10  received 9   loss 10.0%  min/avg/max 11.2/13.4/18.9 ms  jitter 1.7 ms
  TCP   sent 10  received 10  loss 0.0%   min/avg/max 14.0/15.1/17.3 ms  jitter 0.9 ms
  errors: icmp_timeout ×1
  ```

  With `--traceroute`, a hop table follows (`#  IP  hostname  RTT`), then the traceroute error
  if any. Per-sample lines are not printed (that is what `--json` is for).
- **Exit codes:**

  | Code | Meaning |
  | :- | :- |
  | 0 | Measurement ran and at least one ICMP or TCP sample succeeded. |
  | 1 | Measurement ran but no ICMP or TCP sample succeeded. |
  | 2 | Could not run: invalid arguments (clap already uses 2), DNS failure, no address. |

  Traceroute results do not change the exit code; they are reported in the output.
- **Exit 2 with `--json`:** stdout still gets one JSON object,
  `{ "schema_version": 1, "target": "...", "error": { "kind", "message" } }`, so a caller that
  always parses stdout keeps working. clap's own usage errors stay plain text on stderr.
- **JSON style:** both documents (success and exit 2) are pretty-printed with two-space indent,
  keys in the order shown above. Pretty, not compact: a person reads it as easily as `jq` does,
  and one style means one parser path.

### 4. Platform notes

- Windows: ICMP and traceroute use the ICMP Helper API (`IcmpApiProvider`), no admin needed.
- Unix: ping uses unprivileged `SOCK_DGRAM`; if `net.ipv4.ping_group_range` forbids it, every
  ICMP sample is `permission_denied` and TCP still works (exit 0 if TCP succeeded). Traceroute
  needs `SOCK_RAW` (root); without it `traceroute.error.kind = "permission_denied"`. Headless
  never calls `sudo`. Not run by hand in this change (Windows-first).

## Out of scope

- Speed test in headless mode (20 s, heavy traffic, Cloudflare rate limits). Possible later
  behind its own flag.
- JSON Lines / streaming output, `--fail-on` thresholds, Ctrl+C partial reports.
- Changing the TUI's `NetworkStats` loss rule (separate fix; noted above).
- IPv6 targets (assessment §4 item 4).
- Moving `intqual.log` out of the current directory.

## Tests (to be expanded by the test plan)

- Unit, core: `summarize` (all ok, all lost, mixed, one success → jitter null, refused port
  counts as loss); `ProbeError::kind` covers every variant and names are unique;
  report JSON shape (field names, `null` handling, `schema_version`) via `serde_json::to_value`;
  `MeasureRequest::validate` bounds.
- Unit, core, paused time: `measure` with `Source::Mock` returns exactly `count` samples per
  probe, honours the deadline, and returns `interrupted: true` on cancel.
- Integration, loopback: `measure` against a `TcpListener` on `127.0.0.1:0` gives TCP successes;
  a closed port gives `socket` errors and loss 100 %. ICMP to `127.0.0.1` on Windows
  (`#[cfg(windows)]`, no admin).
- CLI, no network: run the built binary (`env!("CARGO_BIN_EXE_intqual")`) with
  `--count 3 --json --mock --interval 10`: valid JSON, `source: "mock"`, exit 0; `--json` without
  `--count` → exit 2; text mode prints the two summary lines.
- Manual (user, Windows, normal shell): `intqual google.com --count 5`, `--json`,
  `--json --traceroute`, and an unresolvable host (`nonexistent.invalid --count 1 --json` →
  exit 2 with JSON error).

## Done when

The manual checks pass in a non-admin shell; build, clippy (`-D warnings`), fmt and tests pass;
the TUI still opens without `--count`; CI green.

## Revisions

- 2026-11-04, after the first manual run: hop `icmp_error` is `null` for Time Exceeded in
  transit; millisecond values and `loss_pct` are rounded to 3 decimals; the exit-2 JSON is
  pretty-printed like the success document. Also fixed outside this spec: the TUI with
  `--mock` froze on `t` because `MockEngine` ignored `StartTraceroute`; it now simulates the
  same three hops as `Source::Mock`.
