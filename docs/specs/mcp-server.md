# Spec: MCP server (`intqual mcp`)

Status: **approved** (not implemented) · Date: 2026-11-04 · Scope: all platforms (verified on Windows only).
Depends on: `docs/specs/headless-mode.md` (the `measure` function and report types).

## Problem

People who use an AI assistant (Claude Desktop, Claude Code, other MCP hosts) cannot ask it to
measure their network with intqual. The assistant would have to shell out and parse text.
MCP (Model Context Protocol) is the standard way to give an assistant typed tools.

## Goal

```
intqual mcp           # MCP server on stdio, real network
intqual mcp --mock    # same tools, simulated data, no network traffic
```

An MCP host starts `intqual mcp` as a child process and gets three tools: `ping`,
`tcp_latency` and `traceroute`. Each one returns the same JSON report as
`intqual --count N --json`, as structured content. No admin rights are needed on Windows.

Registration example (goes in the README):

```bash
claude mcp add intqual -- intqual mcp
```

## Approach

### 1. Library and protocol version

- **rmcp 3.x**, the official Rust SDK (github.com/modelcontextprotocol/rust-sdk). Latest at the
  time of writing is 3.8.0 (2026-10-26). MSRV 1.88; we build with 1.97.
  `rmcp = { version = "3", features = ["server", "macros", "transport-io"] }`
  (`transport-io` provides `rmcp::transport::stdio` and is not a default feature).
- rmcp 3.x implements MCP revision 2026-07-28 and still accepts clients on 2025-11-25 and older
  (initialize handshake), so current Claude clients work.
- JSON Schema for tool inputs and outputs comes from `schemars` 1.x (the version rmcp uses).
- rmcp moves fast: three major versions in 2026 and many types are `#[non_exhaustive]`.
  Use constructors (`CallToolRequestParams::new`, `ProgressNotificationParam::new(..).with_*`),
  not struct literals, and check the CHANGELOG on every bump. `Cargo.lock` pins the exact version.

### 2. Where the code lives

- **New crate `crates/mcp`** (`intqual-mcp`, library), added to the workspace. It depends on
  `intqual-core`, `rmcp`, `schemars`, `serde`, `serde_json`, `tokio`, `tokio-util`, `tracing`.
  It exposes one entry point:

  ```rust
  pub async fn serve_stdio(options: ServerOptions) -> Result<(), McpError>;
  pub struct ServerOptions { pub source: intqual_core::measure::Source }
  ```

  The server type (`IntqualServer`) is also public so tests can serve it on any transport
  (`IntqualServer::serve(transport)`; `serve_stdio` is a thin wrapper). For tests it can also be
  built over a generic measurement function (static dispatch), so a stalling or failing source can
  drive the loss, queueing and cancel tests.
- `crates/core` stays free of MCP and UI code. Its report and request types get
  `#[derive(schemars::JsonSchema)]` behind an optional cargo feature `schema` (off by default);
  `intqual-mcp` turns it on. This avoids a second, hand-copied set of report structs that could
  drift from the headless JSON.
- `crates/cli` depends on `intqual-mcp` and adds an `mcp` subcommand. It initialises logging
  (§6) and calls `serve_stdio`. Headless/TUI code is untouched.
- clap: `Cli` gets an optional subcommand (`args_conflicts_with_subcommands = true`), so
  `intqual mcp` runs the server and `intqual <target>` works as before. A host literally named
  `mcp` must be written `intqual -- mcp` (documented in `--help`).
- **Release:** `intqual-mcp` must be published to crates.io before `intqual`, since the CLI
  depends on it. This spec allows changing `release.yml` to publish `intqual-mcp` after
  `intqual-core` and before `intqual`. The new crate takes the workspace version (1.3.0); no
  other versions change. `PKGBUILD`/`Makefile` need no change (they build the workspace).

### 3. Tools

All three call `intqual_core::measure::measure` and return the full `MeasureReport`
(`schema_version: 1`) as structured content (`rmcp::Json<MeasureReport>`), which also declares
an `outputSchema`. Per the MCP spec, the same JSON is sent as a text block for clients that
ignore structured content. Probes not requested are `null` in the report.

| Tool | Arguments (defaults) | `measure` request |
| :- | :- | :- |
| `ping` | `target` (required), `count` 4, `interval_ms` 500, `timeout_ms` 1000 | `icmp` only |
| `tcp_latency` | `target`, `port` 443, `count` 4, `interval_ms` 500, `timeout_ms` 1000 | `tcp` only |
| `traceroute` | `target`, `max_hops` 30, `timeout_ms` 500 | `traceroute` only |

- Descriptions say what the tool measures, that it sends real traffic to the target, and how
  long it can take, so the model can choose and set expectations. Argument doc comments become
  schema descriptions.
- Annotations (if the rmcp version supports them on `#[tool]`): `readOnlyHint: true`,
  `openWorldHint: true`, `idempotentHint: false`.
- **No speed test tool.** An agent calling it in a loop would saturate the user's line and hit
  Cloudflare rate limits; headless mode excludes it too. Not exposed, not even behind a flag.

### 4. Limits and timeouts

MCP clients put their own timeout on each request (the spec says clients SHOULD, and MAY extend
it on progress). The server keeps every call short and bounded on its own:

- **Stricter than the CLI:** `count` 1..=20, `interval_ms` 100..=5000, `timeout_ms`
  100..=5000, `max_hops` 1..=64, `port` 1..=65535. Checked in the MCP layer first, then
  `MeasureRequest::validate()` runs as usual. Mapping and checks are pure functions
  (`fn to_request(&PingArgs) -> Result<MeasureRequest, ProbeError>` and so on) so they are unit
  tested without a transport.
- **Worst-case budget 60 s per call.** Latency tools: `count × interval + 2 × timeout + 1 s`
  (from the headless deadline) is at most 20 × 5 + 11 = 111 s with extreme arguments, so the
  server also rejects any request whose computed bound is over 60 s (exactly 60 s is
  accepted). Traceroute: `max_hops × 3 × timeout` must be ≤ 60 s (defaults: 30 × 3 × 0.5 s = 45 s). Rejected requests
  get an `invalid_request` tool error that states the bound, so the model can retry smaller.
- **One measurement at a time.** A `tokio::sync::Semaphore(1)` in the server: parallel calls
  queue instead of overlapping. Overlapping ICMP streams would distort each other's RTTs, and it
  stops an agent from flooding the network with parallel calls. Waiting in the queue counts
  toward the client's timeout, not the 60 s budget.
- **Progress:** when the request carries a `progressToken`, the tool passes a channel as
  `measure`'s `progress` argument (headless spec §1) and forwards each `Progress` as
  `notifications/progress` (`progress` = samples/hops done, `total` = `count` or `max_hops`).
  A traceroute that reaches the destination early ends below `total` (no faked 100 %). Without a
  token, `progress` is `None` and nothing is sent. This lets clients that extend
  timeouts on progress keep waiting during a long traceroute.
- **Cancellation:** each tool takes the request context and passes a child of its
  `CancellationToken` (`ctx.ct`) to `measure`. rmcp cancels it on `notifications/cancelled` and on
  stdin EOF (rmcp ≥ 3.6.0). `measure` then stops within one sample/hop timeout. Whether rmcp also
  drops the handler future is unverified, so the tool must not depend on either: it races its own
  work against the token.

### 5. Results and errors

Two kinds of failure, as the MCP spec defines them:

- **Tool execution errors** → a result with `isError: true` and one text block holding
  `{"kind": "...", "message": "..."}` (same shape as in the report). Used when `measure` returns
  `Err` (`invalid_request`, `dns_resolution`, including "no address found"), for the MCP
  limit checks above, and for arguments that do not deserialize (wrong type, missing `target`:
  `invalid_request`). Unknown argument names (a typo such as `cout`) are rejected the same way instead of silently using the default, and numbers outside the field type (`max_hops: 300`) get the bound-naming message. MCP 2025-11-25 makes input errors tool errors so the model can correct
  itself; the handlers deserialize the arguments themselves to keep this one shape (rmcp 3.8
  would otherwise return plain text). The model sees these and can fix its arguments. The handlers build this
  `CallToolResult` themselves rather than returning `Err(String)`, because rmcp would turn a
  plain string into unstructured text, not this JSON shape.
- **Protocol errors** (JSON-RPC error) → only for what rmcp itself rejects: unknown tool,
  malformed JSON-RPC.
- Measurement results with loss, timeouts or `permission_denied` are **not** errors: they are a
  normal result whose report says what happened. (A 100 % loss ping is a valid answer.)
- Cancelled calls: if rmcp still sends a response, it is the partial report with
  `interrupted: true`.

### 6. stdio rules and logging

- stdout is the JSON-RPC stream. Nothing else may write to it: no `println!`, no TUI, no
  tracing output. The `mcp` branch in `main.rs` runs before any terminal setup.
- Logs go to **stderr** (`with_writer(std::io::stderr)`, `with_ansi(false)`), not
  `intqual.log`: an MCP host often starts the server in a directory it cannot write to (for
  example `C:\Windows\System32`), and hosts collect stderr into their own MCP logs. Level from
  `RUST_LOG`, default `warn`.
- If `serve_stdio` fails to start or ends with an error, it is logged and the process exits 1.
  stdin EOF (the host closed the connection) is a normal exit 0.

### 7. Platform notes

- Windows: `ping` and `traceroute` use the ICMP Helper API (`IcmpApiProvider`), no admin, no
  firewall rule needed. `tcp_latency` is a normal TCP connect.
- Unix: `ping` uses unprivileged `SOCK_DGRAM`; where `ping_group_range` forbids it, samples are
  `permission_denied` in the report. `traceroute` needs root, which an MCP child process does not
  have; the report then has `traceroute.error.kind = "permission_denied"` and the tool
  description says traceroute may need root on Linux/macOS. The server never calls `sudo` (there
  is no terminal to type a password into). Not run by hand in this change.

### 8. Server info

`name: "intqual"`, `version: env!("CARGO_PKG_VERSION")`, and short `instructions`: which tool
to use for what, that results are JSON reports with `schema_version`, and that traffic is real
unless the server runs with `--mock`. Capabilities: tools only (no resources, prompts, logging).

## Out of scope

- Speed test tool (see §3).
- HTTP / streamable-HTTP transport (stdio only; a remote server would let others measure *from*
  the user's machine, which needs auth).
- MCP Tasks extension for long-running work (calls are bounded at 60 s).
- Streaming per-sample content, resources (e.g. last report), prompts.
- Publishing to an MCP registry / a `server.json` manifest (later, once the tools are stable).
- Changes to the TUI or headless behaviour.

## Tests (to be expanded by the test plan)

- Unit, `crates/mcp`: argument → `MeasureRequest` mapping for each tool (defaults, `icmp`/`tcp`/
  `traceroute` flags); limit checks and the 60 s budget (boundary values, error text has
  `invalid_request`).
- In-process, `crates/mcp/tests/`: serve `IntqualServer` with `Source::Mock` over
  `tokio::io::duplex`, connect an rmcp client (dev-dependency features `client`,
  `transport-async-rw`): `list_tools` shows exactly `ping`, `tcp_latency`, `traceroute` with
  input and output schemas; each tool returns structured content that deserialises into a report
  with the right probe non-null; invalid arguments give `isError: true`; an unknown tool gives a
  protocol error; two concurrent calls run one after the other; cancel during a call returns or
  ends within one interval; progress notifications arrive when a token is sent (paused time where
  rmcp allows it).
- Loopback: `tcp_latency` against a `TcpListener` on `127.0.0.1:0` with `Source::Network`.
- CLI: spawn `intqual mcp --mock`, send `initialize` + `tools/list` on stdin, check stdout holds
  only JSON-RPC lines and the process exits 0 on stdin EOF.
- Manual (user, Windows, normal shell): `claude mcp add intqual -- <path>\intqual.exe mcp`, then
  ask Claude Code to ping `google.com`, check TCP latency to port 443 and traceroute it.

## Done when

The manual check passes in Claude Code on Windows from a non-admin shell; build, clippy
(`-D warnings`), fmt and tests pass; README documents registration; CI green.
