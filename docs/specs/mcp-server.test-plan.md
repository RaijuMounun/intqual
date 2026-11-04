# Test plan: MCP server

Spec: [mcp-server.md](mcp-server.md) · Date: 2026-11-04 · Produced by the `test-review` skill
(`plan` mode, `test-reviewer` agent). Status: **approved**.

Covers only what the MCP layer adds; `measure` and report tests are in
[headless-mode.test-plan.md](headless-mode.test-plan.md).

## First: spike

`spike_paused_time_over_duplex` (throwaway, in `crates/mcp/tests/`): confirm that
`#[tokio::test(start_paused = true)]` works across `tokio::io::duplex` + an rmcp client, and
whether rmcp drops the handler future on cancel. Rows marked **(R)** depend on it. If paused time
does not work, those rows use a short real interval (≤ 20 ms) with a comment saying why.

## Locations

- **U**: in-file `#[cfg(test)]` in `crates/mcp/src/`.
- **I**: `crates/mcp/tests/server.rs`, in-process (`IntqualServer` with `Source::Mock` over
  `tokio::io::duplex`, rmcp client from dev-dependency features `client`, `transport-async-rw`).
- **C**: `crates/cli/tests/mcp.rs`, spawns `env!("CARGO_BIN_EXE_intqual")`.

## Tests to add

### U: argument mapping, limits, budget

| Test | Proves |
|-|-|
| ping_args_default_map_to_icmp_only_request | count 4, interval 500, timeout 1000; icmp only |
| tcp_args_default_map_to_tcp_only_with_port_443 | defaults + port 443; tcp only |
| tcp_args_explicit_port_is_passed_through | port 8080 reaches the request |
| tcp_args_port_zero_rejected | port 0 → `invalid_request` |
| traceroute_args_default_map_to_traceroute_only | max_hops 30, timeout 500; traceroute only |
| limits_accept_count_1_and_20_reject_0_and_21 | §4 count bounds |
| limits_interval_100_and_5000_pass_99_and_5001_fail | §4 interval bounds |
| limits_timeout_100_and_5000_pass_99_and_5001_fail | §4 timeout bounds |
| limits_max_hops_1_and_64_pass_0_and_65_fail | §4 max_hops bounds |
| budget_latency_exactly_60s_passes_and_60s_plus_1ms_fails | 10×5000 + 2×4500 + 1000 ms = 60 s passes (inclusive) |
| budget_extreme_latency_20x5000_rejected_with_bound_in_text | 111 s → `invalid_request`, text states the 60 s bound |
| budget_traceroute_boundary_cases | 30×3×1000 fails; 30×3×500 passes; 40×3×500 = 60 s passes; 64×3×500 fails |
| limit_check_runs_before_measure_validate | out-of-range value gives the MCP message, not core's |

### I: tool listing and server info

| Test | Proves |
|-|-|
| list_tools_returns_exactly_three_tools | `ping`, `tcp_latency`, `traceroute`; no speed test |
| list_tools_have_input_and_output_schema | required `target`, `outputSchema` present, argument descriptions non-empty |
| tool_descriptions_mention_real_traffic_and_duration | keyword "traffic"; `traceroute` mentions root |
| tool_annotations_read_only_open_world_not_idempotent | only if rmcp supports annotations on `#[tool]` |
| server_info_has_name_version_instructions_tools_only | `intqual`, `CARGO_PKG_VERSION`, instructions non-empty, no resources/prompts/logging |

### I: results and errors

| Test | Proves |
|-|-|
| ping_mock_returns_structured_report_with_icmp_only | structured content → `MeasureReport`, icmp set, tcp/traceroute null, `schema_version` 1 |
| tcp_latency_mock_returns_tcp_only_report | same with tcp set |
| traceroute_mock_returns_three_hops_report | mock hops present, `complete: true` |
| tool_result_text_block_equals_structured_content | text block parses to the same JSON |
| invalid_count_returns_is_error_with_kind_invalid_request | `isError: true`, one text block `{"kind":"invalid_request",...}` |
| over_budget_request_returns_is_error_not_protocol_error | `Ok(CallToolResult)` with `isError`, not `Err` |
| dns_failure_returns_is_error_with_kind_dns_resolution | `Source::Network` + `nonexistent.invalid` (offline-safe) |
| unknown_tool_returns_protocol_error | client gets `Err(ServiceError)` |
| wrong_argument_type_returns_is_error_invalid_request | `count: "x"` → `isError`, kind `invalid_request` (MCP 2025-11-25: the model can correct itself) |
| missing_target_returns_is_error_invalid_request | same for a missing `target` |
| full_loss_ping_is_normal_result_not_error | failing source → `isError: false`, loss 100 |

### I: concurrency, cancellation, progress

| Test | Proves |
|-|-|
| two_concurrent_calls_run_sequentially (R) | second call starts after the first ends |
| cancel_while_queued_releases_semaphore (R) | cancel B while queued; C still succeeds |
| cancel_during_call_returns_interrupted_partial_or_ends (R) | ends within one interval; if a response comes, `interrupted: true`; never hangs |
| stdin_eof_cancels_running_call (R) | drop client mid-call; `waiting()` returns Ok |
| progress_sent_per_sample_when_token_present (R) | progress 1..=count, `total == count` |
| progress_not_sent_without_token | zero notifications |
| traceroute_progress_total_is_max_hops (R) | `total == max_hops`; mock ends at 3 of 30 |

### I: loopback

| Test | Proves |
|-|-|
| tcp_latency_loopback_listener_succeeds | `TcpListener` on 127.0.0.1:0, count 3 → received 3 |
| tcp_latency_closed_port_reports_socket_loss_not_error | `isError: false`, loss 100, kind `socket` |

### C: binary

| Test | Proves |
|-|-|
| cli_mcp_mock_stdout_only_jsonrpc_lines | after `initialize`, `notifications/initialized`, `tools/list`, stdin closed: every stdout line is JSON-RPC 2.0 |
| cli_mcp_stdin_eof_exits_0 | exit 0 within a deadline (`wait` with timeout, no sleep) |
| cli_mcp_logs_to_stderr_not_file | `RUST_LOG=info`, temp cwd: no `intqual.log`, no log lines on stdout |
| cli_mcp_start_failure_exits_1 | needs a forced start failure; if unreliable, manual |
| cli_positional_target_still_works_after_subcommand | `intqual --count 1 --json --mock x` runs headless |
| cli_dash_dash_mcp_is_treated_as_host | `intqual --count 1 --json --mock -- mcp` → target `mcp`, not the server |

### Manual (user, non-admin Windows shell)

1. `claude mcp add intqual -- <path>\intqual.exe mcp`.
2. Ask Claude Code to ping `google.com`, check TCP latency to 443, and traceroute it.
3. README shows the registration command; `release.yml` publishes `intqual-mcp` between core and
   cli; CI green.

## Testability changes (adopted in the specs)

1. Pure `to_request` mapping and limit/budget functions (mcp-server §4).
2. Handlers build the `isError` `CallToolResult` with the `{kind,message}` JSON themselves (§5).
3. `IntqualServer` generic over its measurement function, served on any transport (§2).
4. `measure` takes an optional `Progress` channel (headless-mode §1).
5. Transport injection makes the start-failure test possible; otherwise it is manual.

## Not covered

- `cfg(unix)` behaviour (`permission_denied` traceroute report): not compiled on Windows.
- rmcp API names come from the research notes, not from reading rmcp source; the spike settles
  the (R) rows.
- After the limit and mapping functions exist: `cargo mutants --in-diff` on them.
