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
| H1 | done (`fa40b5e`, `6ea70f8`) | Empty target (`""`) is accepted and pings the local machine (resolves to the host's own address). Must be rejected. |
| H2 | done (`fa40b5e`, `6ea70f8`) | Targets are not validated: `"  google.com  "`, `https://google.com`, `google.com; whoami` and 300-character names all reach DNS and fail as `dns_resolution`. Validate and normalise the target once, in core, for TUI, headless and MCP. |
| H3 | done (`fa40b5e`, `6ea70f8`) | IPv6 is not supported. `::1` fails with a `socket` error on every sample; on a dual-stack network a hostname can resolve to IPv6 and ICMP then fails while TCP works. Add ICMPv6 on Windows (`Icmp6CreateFile` / `Icmp6SendEcho2`, no admin) for ping and traceroute, and `-4` / `-6` address-family selection. Assessment §4 item 4, §5 item 7. |
| H4 | done (`fa40b5e`, `6ea70f8`) | The traceroute 60 s budget error says "use a smaller count, interval or timeout"; traceroute has no count or interval. It should name `max_hops` and `timeout_ms`. |
| H5 | done (`fa40b5e`, `6ea70f8`) | Argument errors read "Invalid request: invalid arguments: …" (double prefix). |

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
| S5 | todo | **Headless speed test** (`--speedtest`, JSON report with Mbps and data used). **Not in MCP** (decided 2026-11-05): an agent calling it in a loop would saturate the user's line and use a lot of data. |
| S6 | todo | Repeated `TrySendError` blocks in `bandwidth.rs` (assessment §5 item 10). |
| S7 | todo | **Bufferbloat test**: keep pinging during the speed test instead of pausing, and grade latency under load (idle vs. loaded RTT), like Cloudflare's and Waveform's tests. The latency increase under load is often what users feel as "laggy internet". |
| S8 | todo | Check `reqwest` use: client reuse and pool size, HTTP/2 vs. HTTP/1.1 for parallel workers, timeouts. Already done: custom User-Agent `intqual-net-tester/1.0`, streamed body counting. Part of S1. |

## TUI overhaul (redesign)

"Overhaul" means a thorough rework, so the word fits: rethink the layout and the visual design,
then refactor the UI code to match. Start with a design spec (mock-ups as text or images) for
the user to approve before any code.

| # | Status | Item |
| :- | :- | :- |
| U1 | todo | **Design spec**: a dashboard layout with progressive disclosure (a "bento grid"): live latency and jitter sparklines, a speed test gauge, the traceroute hop table or map, and an error/status area. Keyboard map, colour themes, small-terminal behaviour. |
| U2 | todo | **Refactor the UI code** for the new design: one state per screen, widgets stay pure render functions, `AppState` tests stay green. Snapshot tests (`insta` + `TestBackend`, a Trial in the test-review checklist) for the new screens. |
| U3 | todo | **Old UI bugs to re-check** before or during the redesign. They come from the old Obsidian list and may already be fixed: the jitter chart does not reach the right edge; after a speed test the ping charts overlap old data instead of resetting; leaving traceroute with Enter and pressing `t` again mixes old and new hop data (the old run must be cancelled and cleared). Reproduce each one, then write a test. |
| U4 | todo | Show background errors in a UI component instead of only logging them. |
| U5 | todo | Optional polish after U1: animations (`tachyonfx`), colour themes. |

## Code quality review

| # | Status | Item |
| :- | :- | :- |
| Q1 | todo | **Code cleanliness review** of the whole workspace: dead code, duplication, naming, module boundaries, error handling (no catch-all `Err(_)` that hides the real cause), and leftover tasks or handles (the "zombie" check: every spawned task ends on cancel, no leaked handles). Use the remaining items in `docs/assessment.md` §5 as the starting list. Output: findings, then small specs. |
| Q2 | todo | **Windows ICMP efficiency.** The ICMP Helper API is the right base: it is what Windows `ping.exe` uses, and it needs no admin. Raw sockets would not be more accurate on Windows; they need admin, and Microsoft restricts them. What can improve: today each echo opens a new handle and blocks one `spawn_blocking` thread (`icmp/windows.rs`). Switch to `IcmpSendEcho2` with an event or APC, so no thread waits per ping, and reuse one handle per probe (assessment §5 item 6). Measure before and after. |
| Q3 | todo | Unix: `send_to` is called directly on the non-blocking socket. When the socket buffer is full it fails with `WouldBlock` and the sample becomes a socket error. Await `async_fd.writable()` before sending. Not compiled on Windows; CI checks it. |
| Q4 | todo | **Library wrappers**: wrap a dependency behind our own trait only at real boundaries where swapping or faking matters (`IcmpProvider` already does this). Wrapping every crate adds code and hides the libraries' own documentation for little gain. Decide case by case in Q1, not as a blanket rule. |

## Later

- Remaining tech debt in `docs/assessment.md` §5 (items 5, 6, 8, 10–14).
- `CoreEngine` and `main.rs` still take the ICMP identifier from the clock; move them to
  `network::icmp::next_identifier()` like `measure`.
- Unix IPv6 (ICMPv6 sockets): deferred, Windows first. Until then Unix returns
  `unsupported` ("IPv6 ICMP is not yet supported on this platform"). Also pass `-4`/`-6` to
  the Unix traceroute worker then.
- Publish the MCP server to an MCP registry (`server.json`) once the tools are stable.
- Multi-target monitoring: ping several hosts (Google, Cloudflare, AWS…) on one screen.
- Traceroute extras: min/max per hop (3 probes are already sent; only the average is shown),
  ASN and GeoIP per hop, UDP and TCP traceroute, scrolling past 30 hops.
- Unix: a friendly privilege prompt instead of the red "sudo required" error (Windows needs
  no admin any more).
- ISP checks: port blocking and DNS manipulation detection.
- Chaos tests: fake providers that return corrupt, late or duplicate replies (the scriptable
  fakes from the test plan are the start).

Done from the old Obsidian list (2026-11-05 check): engine behind a trait (`NetworkEngine`,
`MockEngine`); per-OS ICMP behind `cfg` aliases; library and TUI split into crates; privilege
isolation (Unix sudo worker, Windows needs no admin); English error screen; streamed speed
test body with a custom User-Agent; error kinds instead of "everything is PermissionDenied".
Dropped: switching `mpsc` to `broadcast` (logging already goes through `tracing`, and one
consumer does not need pub-sub); a `HashMap` for metrics (the typed report structs are
clearer, and adding a probe is a small change).
