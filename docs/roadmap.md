# intqual roadmap

The project's to-do list, newest priorities first. Each item gets its own spec in `docs/specs/`
before code (project rule). Older tech-debt items with file:line evidence are in
`docs/assessment.md` §5; this file links to them instead of repeating them.

Status: `todo` · `spec` (spec written) · `doing` · `done` (with commit).

## High priority: problems found in the MCP test (2026-11-05)

Found by a full manual MCP run (40+ cases over stdio JSON-RPC, real network, Windows 11).
Spec: `docs/specs/target-validation-ipv6.md`.

| # | Status | Item |
| :- | :- | :- |
| H1 | spec | Empty target (`""`) is accepted and pings the local machine (resolves to the host's own address). Must be rejected. |
| H2 | spec | Targets are not validated: `"  google.com  "`, `https://google.com`, `google.com; whoami` and 300-character names all reach DNS and fail as `dns_resolution`. Validate and normalise the target once, in core, for TUI, headless and MCP. |
| H3 | spec | IPv6 is not supported. `::1` fails with a `socket` error on every sample; on a dual-stack network a hostname can resolve to IPv6 and ICMP then fails while TCP works. Add ICMPv6 on Windows (`Icmp6CreateFile` / `Icmp6SendEcho2`, no admin) for ping and traceroute, and `-4` / `-6` address-family selection. Assessment §4 item 4, §5 item 7. |
| H4 | spec | The traceroute 60 s budget error says "use a smaller count, interval or timeout"; traceroute has no count or interval. It should name `max_hops` and `timeout_ms`. |
| H5 | spec | Argument errors read "Invalid request: invalid arguments: …" (double prefix). |

Notes, not bugs:

- Windows retries a TCP connect to a closed loopback port for ~2 s, so with a 1 s timeout the
  result is `tcp_timeout`, not a refusal. That is OS behaviour; documented, not changed.
- The traceroute report carries `count: 1`, which traceroute ignores. Kept for JSON schema
  stability (`schema_version: 1`); documented in the spec.
- DNS error `message` text follows the Windows display language (Turkish here). Clients match
  on `kind`, so this is fine.

## Next focus: speed test

The speed test (`crates/core/src/network/bandwidth.rs`, key `s` in the TUI) has had the least
work so far. First step is a review spec covering everything below.

| # | Status | Item |
| :- | :- | :- |
| S1 | todo | **Review the speed test end to end**: method (4 parallel workers, 10 s down, 10 s up, Cloudflare endpoints), accuracy (warm-up/slow-start handling, how Mbps is computed), error paths, cancellation, and compare with how established tools (Cloudflare's own test, Ookla, iperf3) measure. Output: findings plus a spec for the fixes. |
| S2 | todo | **Report data used** by the test (MB downloaded and uploaded, total). Must not affect the measurement: count bytes the code already receives/sends (`total_bytes_downloaded`, `total_up_bytes`), no extra requests or per-chunk work in the hot path. Show it in the TUI result and in the JSON/MCP report. |
| S3 | todo | **Mock speed test** for `--mock`: simulated download/upload progress and a final result, no network traffic, deterministic, so the TUI, headless mode and MCP can be tested offline. |
| S4 | todo | **Testability**: extract the Mbps/progress maths into pure functions and make the base URL a parameter so a local HTTP server (e.g. `wiremock`) can stand in for Cloudflare. Assessment §6 steps 2–3. |
| S5 | todo | **Headless and MCP speed test.** The MCP spec left it out on purpose: an agent calling it in a loop would saturate the user's line and use a lot of data. Revisit after S2: with a data figure, a hard per-call cap, and a cooldown between calls it can be offered safely. Headless `--speedtest` first, MCP tool second. |
| S6 | todo | Repeated `TrySendError` blocks in `bandwidth.rs` (assessment §5 item 10). |

## Later

- Remaining tech debt in `docs/assessment.md` §5 (items 5, 6, 8, 10–14).
- `CoreEngine` and `main.rs` still take the ICMP identifier from the clock; move them to
  `network::icmp::next_identifier()` like `measure`.
- Unix IPv6 (ICMPv6 sockets): deferred, Windows first. Until then Unix returns a clear
  "IPv6 not supported on this platform" error.
- Publish the MCP server to an MCP registry (`server.json`) once the tools are stable.
