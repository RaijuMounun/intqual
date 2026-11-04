# Test plan: Headless mode

Spec: [headless-mode.md](headless-mode.md) · Date: 2026-11-04 · Produced by the `test-review`
skill (`plan` mode, `test-reviewer` agent). Status: **approved**.

Order: pure tests first (summarize, kind, validate, report shape: cheapest, and they pin the JSON
contract the MCP server reuses), then paused-time `measure` tests, then loopback, then CLI.

Rules (CLAUDE.md): no admin, no internet; loopback is fine. New async tests use
`#[tokio::test(start_paused = true)]`, not real timers.

## Locations

- Core unit tests: in-file `#[cfg(test)]` in `measure/summary.rs`, `measure/report.rs`,
  `measure/mod.rs`, `models.rs`.
- Core loopback: `crates/core/tests/measure_loopback.rs`.
- CLI pure functions: in-file in `crates/cli/src/headless.rs`.
- CLI binary: `crates/cli/tests/headless.rs` using `env!("CARGO_BIN_EXE_intqual")`.

## Tests to add

### Core: `summarize`

| Test | Proves |
|-|-|
| summarize_all_ok_computes_min_avg_max_jitter | [10,20,40] → min 10, avg 23.33, max 40, jitter 15, loss 0 |
| summarize_all_lost_gives_100_loss_and_null_stats | received 0 → loss 100.0, min/avg/max/jitter `None` |
| summarize_mixed_counts_each_error_as_loss | 10 sent, 9 received → loss 10.0 |
| summarize_refused_port_socket_error_counts_as_loss | `Socket` counts as loss (differs from the TUI) |
| summarize_dropped_sample_counts_as_loss | `dropped` counts in `sent` and as loss |
| summarize_single_success_has_null_jitter | received 1 → jitter `None`, min/avg/max set |
| summarize_jitter_skips_failed_samples | [10, err, 20] → jitter 10 |
| summarize_empty_input_does_not_divide_by_zero | sent 0 → no NaN/panic; loss 0.0, stats null |

### Core: `ProbeError::kind`, report JSON, `validate`

| Test | Proves |
|-|-|
| probe_error_kind_names_every_variant | Table of all 13 variants (incl. `InvalidRequest` → `invalid_request`) to exact strings |
| probe_error_kind_names_are_unique_snake_case | 13 unique names matching `^[a-z_]+$`; `dropped` distinct from all |
| report_json_matches_golden_for_mock_report | `serde_json::to_value(report)` equals a `json!` literal: keys, `schema_version: 1`, nulls, `source: "mock"` (injected clock) |
| report_json_error_object_has_kind_and_message | `{kind:"icmp_timeout", message:"ICMP Timeout"}` |
| report_json_probe_off_serializes_null | `icmp: false` → `"icmp": null`, tcp present |
| report_json_traceroute_hop_has_hostname_field | Hop keys match the spec, `hostname` nullable; separate check that `TracerouteHop` is unchanged |
| validate_rejects_count_out_of_range | 0, 1001 fail; 1, 1000 pass |
| validate_rejects_interval_out_of_range | 9 ms fails; 10 ms, 60 s pass; 60 s + 1 ms fails |
| validate_rejects_timeout_out_of_range | 99 ms fails; 100 ms, 30 s pass; above 30 s fails |
| validate_rejects_max_hops_out_of_range | 0, 65 fail; 1, 64 pass |
| validate_rejects_both_probes_off | all probes off → `InvalidRequest` |

### Core: `measure` (paused time, mock or injected sources)

| Test | Proves |
|-|-|
| measure_invalid_request_returns_err_without_probing | `Err(InvalidRequest)`, no time advances |
| measure_mock_returns_exactly_count_samples_per_probe | both probes `count` samples, seq 1..=count |
| measure_mock_icmp_off_reports_null_icmp | `icmp: false` → `report.icmp == None`, tcp has `count` |
| measure_mock_tcp_off_reports_null_tcp | same for `tcp: false` |
| measure_mock_skips_dns_for_any_target | `nonexistent.invalid` + Mock → `Ok`, `resolved_ip == "192.0.2.1"` |
| measure_mock_is_deterministic | two runs → equal JSON (injected clock) |
| measure_mock_waits_interval_between_samples | elapsed ≥ (count−1)×interval and within the deadline |
| measure_mock_traceroute_returns_three_fixed_hops | 3 hops in 192.0.2.0/24, `complete: true`, last is destination |
| measure_deadline_records_missing_samples_as_dropped | stalling source → missing samples listed (seq, `rtt_ms` null, `dropped`, `timestamp == started_at`), `sent == count`, returns at `count×interval+2×timeout+1s` |
| measure_cancel_returns_partial_report_interrupted | cancel after k → `interrupted: true`, k samples, `sent == k`, no `dropped` |
| measure_cancel_during_traceroute_keeps_hops_incomplete | hops kept, `complete: false`, `interrupted: true` |
| measure_not_cancelled_reports_interrupted_false | negative case |
| measure_traceroute_failure_is_data_not_err | failing trace source → `Ok(report)`, `traceroute.error.kind`, kept hops |
| measure_traceroute_runs_after_ping_tcp | all samples before the first hop |
| measure_progress_reports_each_tick_and_hop | mock + channel → latency `done` 1..=count with `total == count`, then hops with `total == max_hops`; no faked final `done == total` |
| measure_progress_full_channel_does_not_block | capacity-1 channel never read → `measure` still returns within its bound |
| measure_progress_none_is_accepted | `None` → same report as with a channel |
| validate_rejects_port_zero_when_tcp_on | port 0 + tcp → `InvalidRequest`; port 0 + tcp off → ok |
| measure_unresolvable_target_returns_dns_error | Network + `nonexistent.invalid` → `Err(DnsResolution)` (offline-safe, RFC 6761) |

### Core: loopback integration

| Test | Proves |
|-|-|
| measure_loopback_tcp_listener_succeeds | `TcpListener` on 127.0.0.1:0 → received == count, loss 0 |
| measure_closed_port_gives_socket_errors_and_full_loss | bind+drop for a free port → `kind:"socket"`, loss 100 |
| measure_loopback_icmp_succeeds_on_windows | `#[cfg(windows)]`, ICMP to 127.0.0.1 → received ≥ 1 |

### CLI: pure functions (`headless.rs`)

| Test | Proves |
|-|-|
| exit_code_is_0_when_only_tcp_succeeds | TCP-only 0, ICMP-only 0, all failed 1 |
| exit_code_ignores_traceroute_result | failing traceroute doesn't change 0 or 1 |
| render_text_prints_header_and_two_summary_lines | exact text for a fixed report, incl. `errors: icmp_timeout ×1` |
| render_text_with_traceroute_prints_hop_table_and_error | hop rows, then the traceroute error line |

### CLI: binary (no network)

| Test | Proves |
|-|-|
| cli_json_mock_prints_valid_json_exit_0 | `--count 3 --json --mock --interval 10` → one JSON document, `source == "mock"`, 3 samples, exit 0 |
| cli_json_without_count_exits_2 | `--json` / `--traceroute` alone → exit 2, plain stderr |
| cli_invalid_count_exits_2_with_json_error | `--count 0 --json` → exit 2, stdout error object `invalid_request` |
| cli_dns_failure_json_exits_2_with_error_object | `nonexistent.invalid --count 1 --json` → exit 2, `dns_resolution` |
| cli_dns_failure_text_writes_stderr_only | exit 2, stdout empty, stderr non-empty |
| cli_text_mode_mock_prints_summary_lines | stdout has `ICMP  sent 3` and `TCP   sent 3` |
| cli_headless_does_not_enter_alternate_screen | stdout has no `\x1b[?1049h` |

### Manual (user, non-admin Windows shell)

- `intqual --mock` (no `--count`) still opens the TUI.
- `intqual google.com --count 5`, `--json`, `--json --traceroute`, and
  `nonexistent.invalid --count 1 --json` (exit 2, JSON error).

## Existing tests to touch

- `crates/core/src/engine/mock.rs`: when `sample` moves to the shared function, add
  `sample_returns_sawtooth_values` (e.g. `sample(1) == (Ok(27.0), Ok(32.0))`); the current test
  checks only the every-20th loss.

## Testability changes (all adopted in the spec)

1. `MockEngine::sample` → shared pure function.
2. Generic collect step and trace source (static dispatch) for the `dropped`, cancel-in-traceroute
   and traceroute-failure tests.
3. Pure `exit_code`, `render_text`, and `run(.., impl Write)` in `headless.rs`.
4. Injectable clock for `started_at` and timestamps.
5. `serde_json` dev-dependency in core and cli.

## Not covered

- `cfg(unix)` behaviour (`permission_denied`, root traceroute): not compiled on Windows; CI
  compiles it, behaviour stays manual.
- Injection points are proposals; the agent did not read probe internals.

## Revision 2026-11-04 (after the first manual run)

| Test | Proves |
|-|-|
| classify_time_exceeded_in_transit_is_a_normal_hop | Type 11 code 0 → `(false, None)`: not an error |
| classify_fragment_reassembly_time_exceeded_stays_an_error | Type 11 code 1 keeps `"Type: 11, Code: 1"` |
| classify_echo_reply_is_the_destination_without_error | Echo Reply → `(true, None)` |
| classify_destination_unreachable_keeps_the_error | Type 3 → destination, error kept (replaced below) |
| classify_unknown_type_keeps_the_error | unknown type → error kept |
| round_ms_keeps_three_decimals | `82.27239999999999` → `82.272` |
| sample_rtt_is_rounded_in_json | `rtt_ms` serializes as `82.272` |
| report_hop_from_hop_rounds_rtt_and_keeps_fields | hop RTT rounded, other fields unchanged |
| summarize_rounds_stats_to_three_decimals | avg `1.667`, loss `33.333` (`summarize_all_ok…` now expects avg `23.333`) |
| run_json_success_and_error_are_both_pretty_printed | both documents start `{
  "schema_version": 1,` |
| start_traceroute_emits_hops_dns_and_complete | `MockEngine` on Pause + StartTraceroute: 3 hops, a hostname each, Complete, no pings meanwhile |
| resume_after_traceroute_restarts_pings | Resume (Esc/Enter) after the trace → pings again |
| stop_during_traceroute_ends_it_and_closes_the_channel | Stop mid-trace → channel closes, no hops |
| second_start_traceroute_replaces_the_first | two StartTraceroute → exactly 3 hops, one Complete |
| measure_rounds_hop_rtt_in_the_report | a `1.00049` ms hop is `1.0` in the report (rounding wired into `collect_traceroute`) |
| resume_during_traceroute_stops_remaining_hops | Resume mid-trace → no more hops or Complete leak into the ping view |

Manual: `intqual --mock`, press `t` (three hops with hostnames), then Esc (chart moves again);
`intqual google.com --count 5 --json --traceroute` (intermediate hops have `icmp_error: null`,
values have at most 3 decimals).

## Revision 2026-11-04 (test review gaps)

| Test | Proves |
|-|-|
| summarize_jitter_does_not_bridge_failed_samples | `[10, err, 20]` → jitter `null`; `[10, 20, err, 40, 45]` → `7.5` (renamed from `summarize_jitter_skips_failed_samples`) |
| summarize_jitter_uses_absolute_consecutive_differences | `[10, 30, 20, 40]` → `16.667` (a missing `abs` gives 10) |
| measure_pre_cancelled_returns_interrupted_empty_report | cancelled before the call → `Ok`, interrupted, no samples, no hops, no time spent |
| measure_stops_latency_probes_on_every_exit | latency token cancelled after a cancel, the deadline and an early channel close |
| measure_probe_channel_closed_early_marks_rest_dropped | sources gone after seq 1 → seq 2..=count `dropped`, returns at once |
| measure_fatal_event_is_logged_and_collection_continues | `Fatal` is logged at ERROR and all samples still arrive |
| measure_ignores_duplicate_and_out_of_range_seq | seq 0, a second seq 1 and seq `count + 1` change nothing |
| measure_reverse_dns_timeout_leaves_hostname_null | a stalled lookup costs one `timeout` per hop, hostname `null` |
| measure_hop_without_ip_skips_reverse_dns | a `*` hop gets no lookup |
| measure_cancel_during_reverse_dns_keeps_hop | cancel mid-lookup keeps the hop, `complete: false` |
| measure_traceroute_task_panic_is_thread_panic_error | a panicking traceroute task → `error.kind = "thread_panic"` |
| measure_unreachable_from_router_is_not_complete | a hop with `is_destination: false` and a Type 3 error → `complete: false` |
| classify_destination_unreachable_from_a_router_stops_without_reaching | router's Type 3 → stop, not reached (was: reached) |
| classify_destination_unreachable_from_the_target_counts_as_reached | target's own Type 3 → reached |
| next_identifier_never_repeats_within_a_process | 10 000 calls, 10 000 distinct identifiers |
| next_identifier_is_distinct_across_threads | 8 threads × 500 calls, no repeat |
| measure_loopback_two_concurrent_measures_do_not_cross_talk | two concurrent measures (TCP, plus ICMP on Windows) each get exactly their own `count` samples |
| report_json_matches_golden_with_traceroute | full document with transit hop (`icmp_error: null`, hostname set), `*` hop and destination |
| cli_headless_does_not_enter_alternate_screen | now also exit 0 and non-empty stdout |

`render_text_prints_header_and_two_summary_lines` now expects `jitter - ms` for ICMP
`[11, err, 13]` (no adjacent pair).

`cli_closed_port_exits_1` needed a TCP-only run (loopback ICMP always succeeds), so the CLI got
`--no-icmp` (spec revision 2026-11-05):

| Test | Proves |
|-|-|
| cli_closed_port_exits_1 | `127.0.0.1 --port <closed> --count 1 --no-icmp --json` → exit 1, `icmp: null`, one `socket` error |
| cli_no_icmp_reports_null_icmp | `--no-icmp --mock` → `icmp: null`, TCP samples present |
| cli_json_without_count_exits_2 | now also covers `--no-icmp` without `--count` |
