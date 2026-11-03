# intqual — Project Assessment

Date: 2026-11-03 · Commit: `49a10cb` (master, clean) · Assessed on Windows 11, rustc/cargo 1.97.1.
Scope: read-only. No source, Cargo or workflow file was changed.

> Line numbers refer to commit `49a10cb`; later fix commits shift them.

---

## 1. What each module does

Workspace (`Cargo.toml:1-6`), two crates, both `edition = "2024"`, version 1.3.0.

### `crates/core` — `intqual-core` (library)

| Path | Role |
|---|---|
| `crates/core/src/lib.rs:1-5` | Exposes `models`, `engine`, `utils`, `probe`; `network` is private. |
| `crates/core/src/models.rs:3-29` | `ProbeError` (thiserror enum, 12 variants). |
| `crates/core/src/models.rs:35-61` | `PingMetrics` — last-tick snapshot used by the UI. |
| `crates/core/src/models.rs:63-80` | `BandwidthProgress` (Downloading/Uploading/Finished/Failed) and `TracerouteHop` (serde, used for IPC). |
| `crates/core/src/probe/mod.rs:7-18` | `TelemetryEvent` — the single message type from engine to UI. |
| `crates/core/src/probe/mod.rs:20-22` | `NetworkProbe` trait (`run(tx, cancel_token)`). |
| `crates/core/src/probe/ping.rs:24-80` | ICMP echo every `interval`, sends `TelemetryEvent::Ping`. |
| `crates/core/src/probe/tcp.rs:24-130` | TCP connect-time ("TCP ping"), RST-close via `SO_LINGER=0` (`tcp.rs:52-60`), sends `TelemetryEvent::Tcp`. |
| `crates/core/src/probe/traceroute.rs:23-149` | TTL 1..=30, 3 probes per hop, averages RTT, sends `TracerouteHop` then `TracerouteComplete`. |
| `crates/core/src/engine/mod.rs:8-10` | `NetworkEngine` trait (`start(tx, cmd_rx)`). |
| `crates/core/src/engine/core_engine.rs:9-16` | `EngineCommand` (Pause/Resume/Stop/StartBandwidthTest/StartTraceroute). |
| `crates/core/src/engine/core_engine.rs:37-186` | Resolves DNS, spawns a command loop that starts/stops probes with `CancellationToken`s. |
| `crates/core/src/engine/mock.rs:11-22` | `MockEngine` — only logs commands; never sends telemetry. |
| `crates/core/src/network/bandwidth.rs:9-353` | Speed test against `speed.cloudflare.com`: 4 parallel download workers (10 s), then 4 upload workers (10 s). |
| `crates/core/src/network/icmp/packet.rs` | ICMP encode (`:29-66`), RFC 1071 checksum (`:71-96`), decode of Echo Reply (`:116-146`), Time Exceeded / Dest Unreachable (`:183-256`), IPv4 header strip (`:172-181`). |
| `crates/core/src/network/icmp/provider.rs:12-15` | `IcmpProvider` trait: `ping`, `send_with_ttl`. |
| `crates/core/src/network/icmp/mod.rs:17-27` | Per-OS type aliases `DefaultIcmpProvider` / `TracerouteIcmpProvider`. |
| `crates/core/src/network/icmp/unix.rs` | Unix unprivileged `SOCK_DGRAM` ICMP (ping). |
| `crates/core/src/network/icmp/unix_raw.rs` | Unix `SOCK_RAW` ICMP (traceroute; needs root). |
| `crates/core/src/network/icmp/windows.rs` | Windows `SOCK_RAW` ICMP for both ping and traceroute; blocking recv in `spawn_blocking`. |
| `crates/core/src/utils.rs:4-9` | `current_timestamp()` (UNIX seconds). |
| `crates/core/scratch.rs:1-3` | Leftover experiment, not part of any target (not listed in `Cargo.toml`). |

### `crates/cli` — `intqual` (binary)

| Path | Role |
|---|---|
| `crates/cli/src/main.rs:9-41` | clap CLI: `target`, `--port`, `--interval`, `--timeout`, `--mock`, hidden `--worker-mode`. |
| `crates/cli/src/main.rs:62-117` | Worker mode: runs `TracerouteProbe`, prints each hop as one JSON line to stdout. |
| `crates/cli/src/main.rs:119-139` | Normal mode: creates channels, starts the engine, hands over to `ui::run_app`. |
| `crates/cli/src/ui/mod.rs:44-90` | `NetworkStats` — loss %, jitter, min/max. |
| `crates/cli/src/ui/mod.rs:92-323` | `AppState` and `handle_event` (state machine driven by `TelemetryEvent`). |
| `crates/cli/src/ui/mod.rs:326-537` | `run_app` — synchronous render/input loop; keys q/s/t/Esc/Enter; spawns the sudo traceroute worker (`:404-496`). |
| `crates/cli/src/ui/widgets/mod.rs:4-21` | `ActiveWidget` enum dispatch. |
| `crates/cli/src/ui/widgets/{latency,bandwidth,traceroute,error}.rs` | ratatui render functions, read-only over `AppState`. |

Release tooling: `.github/workflows/release.yml` (tag `v*` → build 4 targets → GitHub Release → crates.io → winget), `Makefile` (`ready`, `distribute`), `PKGBUILD` (Arch), `scripts/distribute.sh` (updates the Homebrew tap).

---

## 2. Data flow: probe → UI

```
main.rs:120  mpsc::channel(100)  TelemetryEvent      (engine → UI)
main.rs:121  mpsc::channel(10)   EngineCommand       (UI → engine)

CoreEngine::start (core_engine.rs:37)
  ├─ tokio::net::lookup_host  → first address only (core_engine.rs:39-42)
  ├─ icmp_identifier from clock nanos (core_engine.rs:82-83)
  └─ spawn command loop (core_engine.rs:94-185)
        └─ spawn_probes (core_engine.rs:190-223)
              ├─ PingProbe::run → DefaultIcmpProvider::new(..).ping(..)  (ping.rs:45-48)
              │     └─ tx.try_send(TelemetryEvent::Ping{..})             (ping.rs:65)
              └─ TcpProbe::run  → TcpStream::connect with timeout       (tcp.rs:45-48)
                    └─ tx.try_send(TelemetryEvent::Tcp{..})              (tcp.rs:115)

ui::run_app (ui/mod.rs:326)
  loop:
    drain rx.try_recv()  → AppState::handle_event  (ui/mod.rs:345-360, 174-322)
    if changed → terminal.draw(draw_ui)             (ui/mod.rs:366-368, 540-575)
    event::poll(50 ms) → key → cmd_tx.try_send(EngineCommand::..)  (ui/mod.rs:371-531)
```

- Back-pressure: producers use `try_send` and drop frames when the channel is full (e.g. `ping.rs:65-75`). Fatal errors use `send().await`.
- Speed test: key `s` → `StartBandwidthTest` → engine cancels ping/TCP and spawns `bandwidth::test_download` (`core_engine.rs:126-156`). Esc/Enter → `Resume` cancels the test and restarts probes (`core_engine.rs:107-116`).
- Traceroute does **not** use the engine path. Key `t` sends `Pause`, leaves the alternate screen, runs `sudo -v`, then spawns `sudo -E <self> --worker-mode traceroute <ip>` (`ui/mod.rs:399-429`). The child prints JSONL hops (`main.rs:100-103`); the parent parses each line (`ui/mod.rs:448-449`), does reverse DNS in `spawn_blocking` (`ui/mod.rs:454-467`), and forwards `TracerouteHop` / `DnsResolved` / `TracerouteComplete` into the same telemetry channel.
- `EngineCommand::StartTraceroute` (`core_engine.rs:157-182`) is never sent by the UI — dead path.

---

## 3. Build / test / lint status (real output, Windows)

| Command | Result |
|---|---|
| `cargo build --workspace` | OK — `Finished dev profile ... in 47.41s` (first build). |
| `cargo test --workspace` | OK but empty — `running 0 tests` for `intqual` (main.rs), `intqual_core` (lib.rs) and doc-tests. **There are no tests.** |
| `cargo clippy --workspace --all-targets -- -D warnings` | OK — no warnings. **Caveat:** on Windows, `unix.rs` and `unix_raw.rs` are `#[cfg(unix)]` and are not compiled or linted (`icmp/mod.rs:5-9`). Only the `x86_64-pc-windows-msvc` target is installed (`rustup target list --installed`). |
| `cargo fmt --all -- --check` | **FAILS** (exit 1) — 210 diff hunks across every `.rs` file in both crates. The code has never been formatted with rustfmt. |
| `cargo run -p intqual -- --help` | OK — prints help. Side effect: creates `intqual.log` in the current directory (`main.rs:48`). |
| `cargo tree --workspace --duplicates` | Only transitive duplicates (`darling` 0.20/0.23, `hashbrown` 0.16/0.17, `itertools`) — harmless. |

Not run (by constraint): the real TUI, ICMP/traceroute (raw sockets need admin), the speed test (network).

CI: only `release.yml`, triggered by tags. It builds (`release.yml:56`, no `--locked`) but never runs `fmt`, `clippy` or `test`. There is no CI on push/PR.

---

## 4. Platform-specific code and its risks

| Platform | Ping provider | Traceroute provider | Privilege |
|---|---|---|---|
| Linux/macOS | `UnixDgramIcmp` (`icmp/mod.rs:17-18`) | `unix_raw::RawIcmpProvider` (`:20-21`) | Ping: unprivileged if `net.ipv4.ping_group_range` allows it. Traceroute: root via sudo worker. |
| Windows | `windows::RawIcmpProvider` (`:23-24`) | same (`:26-27`) | **Both need Administrator.** |

Risks:

1. **Windows ping needs admin.** `windows.rs:28` opens `SOCK_RAW`; without elevation it returns `PermissionDenied` every tick, and the UI shows "Perm Denied" (`latency.rs:29-32`). This contradicts "unprivileged" in `PKGBUILD:5` and the README architecture section. The standard unprivileged API on Windows is `IcmpSendEcho2` (iphlpapi), which `ping.exe` and `tracert.exe` use; it also supports TTL, so it would fix traceroute too.
2. **Windows traceroute calls `sudo`** (`ui/mod.rs:407`, `:421`). Windows 11 `sudo` is off by default and does not accept `-v`/`-E` the same way. The README admits it is broken, yet `release.yml:158-170` still publishes to winget.
3. **Windows timeout is per `recv`, not per ping.** `set_read_timeout` (`windows.rs:52`) restarts on every foreign ICMP packet in the loop (`windows.rs:68-96`). On a busy host one ping can wait much longer than `--timeout`.
4. **IPv4 only.** Every socket is `Domain::IPV4` (`windows.rs:28`, `unix.rs:28`, `unix_raw.rs:28`), but the engine takes the first DNS answer (`core_engine.rs:41`), which can be IPv6 on dual-stack networks. ICMP then fails with a socket error for the whole session. TCP works with IPv6, so the two layers would disagree.
5. **macOS DGRAM replies (unverified).** On BSD-derived kernels, `SOCK_DGRAM` ICMP reads usually include the IP header. `unix.rs:79` decodes without `strip_ipv4_header`, unlike `unix_raw.rs:92` and `windows.rs:74`. If this holds, every macOS ping is dropped as "not an Echo Reply" and times out. Needs a test on a Mac.
6. **`unsafe` without `// SAFETY:` comments.** 6 blocks turn `[MaybeUninit<u8>]` into `&[u8]` after `recv_from` (`windows.rs:71`, `:162`; `unix.rs:75`, `:168`; `unix_raw.rs:88`, `:196`). They are sound — `recv_from` initialises the first `size` bytes — but the invariant is not written down (`grep SAFETY` finds 0). Standard practice: a `// SAFETY:` comment on every `unsafe` block, and `#![deny(clippy::undocumented_unsafe_blocks)]` to enforce it. Better: one safe helper used by all three providers.
7. **Unix code is never linted on the dev machine.** See §3. CI on `ubuntu-latest` and `macos-latest` would cover it.
8. **Worker runs as root and writes `intqual.log` in the CWD** (`main.rs:48`), which can leave a root-owned log file in the user's directory.

---

## 5. Tech debt and refactor candidates (ranked by risk)

High

1. **TLS verification disabled** — `bandwidth.rs:20` `.danger_accept_invalid_certs(true)`. Anyone on the path can intercept the speed-test traffic. `rustls-tls` is already enabled (`Cargo.toml:25`), so removing this line should just work. Security issue; fix first.
2. **`--mock` hangs before the UI starts** — `MockEngine::start` awaits `cmd_rx.recv()` until `Stop` (`mock.rs:14-19`), and `main.rs:127` awaits it *before* `ui::run_app` (`main.rs:134`). No one can send `Stop`, so the process hangs. It also never sends telemetry (`_tx`, `mock.rs:12`). The one tool meant for offline testing does not work.
3. **Windows privilege model** — §4 items 1–2.
4. **Traceroute worker hides errors** — if `probe.run` fails (e.g. `PermissionDenied`), the worker only logs it (`main.rs:93-95`); `tx` is dropped, the loop ends and the worker exits 0 (`main.rs:116`). The parent then reports `TracerouteComplete` (`ui/mod.rs:478-479`). The user sees "Complete" with no hops instead of an error.

Medium

5. **Three near-copies of the ICMP code** — `unix.rs` (227 lines), `unix_raw.rs` (257), `windows.rs` (223) repeat socket creation, matching and decoding. `UnixDgramIcmp::send_with_ttl` (`unix.rs:113-226`) is never called (the Unix traceroute alias is `unix_raw`, `icmp/mod.rs:21`). Extract shared matching (`is_match`, `got_id`) into `packet.rs`.
6. **Probes hard-code their provider** — `PingProbe` calls `DefaultIcmpProvider::new` (`ping.rs:46`) and `TracerouteProbe` calls `TracerouteIcmpProvider::new` (`traceroute.rs:46`). This blocks unit tests (no way to inject a fake), and it opens a new socket — plus, on Windows, a blocking thread — on every tick. Make the probes generic over `P: IcmpProvider`, created once.
7. **IPv6** — §4 item 4. Either filter for IPv4 at resolve time or add ICMPv6.
8. **Dead traceroute path in the engine** — `core_engine.rs:157-182`: the command is never sent; its token is created but never stored, so `Stop` could not cancel it; the `_target` argument is ignored. Remove it or make the worker use it.
9. **Error classification** — TCP "connection refused" becomes `ProbeError::Socket` (`tcp.rs:76-78`), which `NetworkStats::update` neither counts as loss nor as success (`ui/mod.rs:81-83`), so loss % is wrong for closed ports. Fatal errors use `Fatal` in some places and `TracerouteError` in others (`core_engine.rs:177` vs `ui/mod.rs:433`).

Low

10. **Repeated `TrySendError` match** — the same 10-line block appears at `ping.rs:65-75`, `tcp.rs:115-125`, `traceroute.rs:117-127`, `:137-147`, `bandwidth.rs:161-171`, `:308-318`, `:343-350`, `core_engine.rs:67-77`, `:144-153`. One helper function would remove ~80 lines.
11. **Unused things** — core deps `tokio-rustls`, `rustls-pki-types`, `webpki-roots`, `anyhow`, `serde_json` (`crates/core/Cargo.toml:14-16,20,25`; no use in `crates/core/src`); `#![allow(unused_imports)]` (`icmp/mod.rs:1`) hides them; `_cmd_tx` unused (`ui/mod.rs:174`); `scratch.rs`.
12. **Version and reproducibility drift** — CLI depends on `intqual-core` `version = "1.0.9"` while the crate is 1.3.0 (`crates/cli/Cargo.toml:11`); the release build has no `--locked` (`release.yml:56`) while `PKGBUILD:17` does; `make ready` runs `cargo update` (`Makefile:6`), so every local build can pull new versions; no `rust-version` field although edition 2024 needs Rust ≥ 1.85.
13. **Inaccurate text** — clap `about` says "zero-runtime-dependency" (`main.rs:10`); the error widget always says sudo is required, whatever the error (`error.rs:24`).
14. **`main` returns `Result<(), ()>`** (`main.rs:47`) — standard is `anyhow::Result<()>` in binaries (anyhow is already a workspace dep) so errors print properly; libraries keep `thiserror` (already done in `models.rs`).
15. **Formatting** — §3; run `cargo fmt --all` once in its own commit so later diffs stay readable.
16. **`calculate_checksum` underflows on an empty buffer** — `buffer.len() - 1` (`packet.rs:76`). Callers always pass ≥ 8 bytes today, but a test with `&[]` would panic.

---

## 6. Testing plan

Rule for all tests: **no admin rights and no internet.** Loopback (`127.0.0.1`) is allowed. Anything that needs either goes behind `#[ignore]` and is run by hand (`cargo test -- --ignored`).

### Step 1 — pure unit tests (no refactor needed, start here)

In-file `#[cfg(test)] mod tests` next to the code:

- `packet.rs`: `encode()` produces the right bytes and a checksum that verifies to 0; RFC 1071 example vector; odd-length payload; `IcmpEchoReply::decode` on short / wrong-type / valid buffers; `IcmpResponse::decode` on hand-built Time Exceeded and Dest Unreachable packets (8-byte ICMP + 20-byte IPv4 + 8-byte original ICMP), including IHL > 5 and non-ICMP protocol; `strip_ipv4_header` with and without an IP header. Highest value: this is the code most likely to break silently across platforms.
- `ui/mod.rs` `NetworkStats::update`: loss %, jitter, min/max over a scripted sequence of `Ok`/`Err`.
- `ui/mod.rs` `AppState::handle_event`: sliding window stays at `HISTORY_SIZE`; `Fatal` switches to `AppMode::Error`; `BandwidthError` is ignored outside bandwidth mode; `TracerouteHop` sets DNS status to `Resolving`.
- `models.rs` `TracerouteHop`: serde JSON round-trip (this is the IPC contract between worker and parent).

### Step 2 — small refactors that unlock tests (each gets its own spec)

- Make `PingProbe` / `TracerouteProbe` generic over `IcmpProvider` (§5 item 6). Then a `FakeIcmpProvider` returning scripted results lets you test sequence numbers, timeout handling, hop aggregation, "stop at destination" and fatal-error propagation with no sockets.
- Extract the Mbps/progress maths from `bandwidth.rs:150-156` and `:331-337` into pure functions.
- Make `MockEngine` emit scripted telemetry (and not block `main`), so it can drive the UI in tests.

### Step 3 — integration tests (`crates/*/tests/`, loopback only)

- `TcpProbe` against `tokio::net::TcpListener::bind("127.0.0.1:0")`: success path, refused port (bind then drop the listener), timeout (use `tokio::time::pause()` where possible).
- `CoreEngine` command loop with a fake probe: Pause stops events, Resume restarts them, Stop ends the loop. Use `#[tokio::test(start_paused = true)]` to avoid real waiting.
- `bandwidth::test_download` against a local HTTP server (e.g. `wiremock` or a tiny `hyper` server) — requires the URL scheme/host to become a parameter instead of `https://` + Cloudflare (`bandwidth.rs:30`, `core_engine.rs:136-137`).
- Worker IPC: run the parent-side JSONL parser on a fixed byte stream (needs the parser moved out of `run_app`).

### Step 4 — UI render tests

ratatui `TestBackend` + snapshot crate (`insta`) for each widget with a fixed `AppState`. Catches layout regressions without a terminal.

### Needs network or admin (manual, `#[ignore]`)

Real ICMP on Windows (admin) and Linux (`ping_group_range`), real traceroute, the Cloudflare speed test, reverse DNS. Run before a release, not in the loop.

### CI to add alongside

A `ci.yml` on push/PR with a matrix `ubuntu-latest`, `windows-latest`, `macos-latest` running `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked`. The matrix is what lints the Unix-only code (§4 item 7).
