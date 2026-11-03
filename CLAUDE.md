# intqual

Terminal (TUI) network analyzer in Rust. Measures ICMP ping and TCP connect latency side by side,
plus a speed test (Cloudflare) and traceroute. Cargo workspace, edition 2024, Tokio + ratatui.
Full analysis, known bugs and the testing plan: `docs/assessment.md`.

## Commands (verified on Windows 11, cargo 1.97.1)

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p intqual -- --help
```

- The repo is rustfmt-clean; keep `cargo fmt --all -- --check` passing.
- `cargo run -p intqual` (the TUI) sends real network traffic and writes `intqual.log` to the
  current directory. `--mock` shows simulated ping/TCP data with no network traffic.
- Windows ICMP uses raw sockets (`icmp/windows.rs`). A non-elevated loopback traceroute works;
  remote hosts are unverified. Never assume admin is available.
- Clippy on Windows does not compile `#[cfg(unix)]` files (`icmp/unix.rs`, `icmp/unix_raw.rs`).

## Layout

- `crates/core` (`intqual-core`, lib): no UI code.
  - `models.rs` — `ProbeError` (thiserror), `PingMetrics`, `BandwidthProgress`, `TracerouteHop`.
  - `probe/` — `NetworkProbe` trait, `TelemetryEvent`; `ping.rs`, `tcp.rs`, `traceroute.rs`.
  - `engine/` — `NetworkEngine` trait, `EngineCommand`, `CoreEngine`, `MockEngine`.
  - `network/icmp/` — `packet.rs` (encode/decode/checksum), `provider.rs` (`IcmpProvider`),
    per-OS providers `unix.rs`, `unix_raw.rs`, `windows.rs`, selected by aliases in `mod.rs`.
  - `network/bandwidth.rs` — speed test.
- `crates/cli` (`intqual`, bin): `main.rs` (clap, hidden `--worker-mode traceroute`),
  `ui/mod.rs` (`AppState`, event loop), `ui/widgets/*` (pure render functions).
- `crates/core/scratch.rs` — leftover, not built. CI: `.github/workflows/ci.yml` (fmt, clippy,
  test on Linux/Windows/macOS; the only place Unix code gets linted). Release: `release.yml`,
  `Makefile`, `PKGBUILD`, `scripts/distribute.sh`.

Data flow: probes → `mpsc::Sender<TelemetryEvent>` (cap 100) → `AppState::handle_event` → widgets.
UI → `mpsc::Sender<EngineCommand>` (cap 10) → engine loop. Traceroute runs in a `sudo` child
process that prints `TracerouteHop` as JSON lines.

## Conventions observed

- Errors: library returns `Result<_, ProbeError>`; failures are data (`Result<f64, ProbeError>`
  inside events), not panics. No `unwrap()`/`expect()` outside `#[cfg(test)]` — keep it that way.
- Every error path logs with `tracing` (`error!`/`warn!`) before returning.
- Telemetry uses `try_send` and drops the frame on `Full`; stop the task on `Closed`.
  Fatal errors use `send().await`.
- Tasks stop through `tokio_util::sync::CancellationToken`, never by aborting.
- Static dispatch: traits with `async fn`/`impl Future`, no `dyn`, no `async-trait` crate.
- Per-OS code lives behind `#[cfg(unix)]` / `#[cfg(target_os = "windows")]` modules and
  type aliases in `network/icmp/mod.rs`, not inline `cfg` in probe logic.
- Doc comments explain *why* (see `models.rs`, `main.rs`).

## Rules for agents

- Before saying a change is done: `cargo build --workspace`, `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` must pass. Paste the result.
- Run `cargo fmt --all` before every commit.
- Tests must never need Administrator/root or the internet. Loopback (`127.0.0.1`) is fine.
  Anything that does need them gets `#[ignore]` with a comment saying why.
- Test ICMP logic through `packet.rs` byte buffers or a fake `IcmpProvider`, not real sockets.
- Every new or changed `unsafe` block gets a `// SAFETY:` comment stating the invariant.
- Do not touch `#[cfg(unix)]` code without saying it was not compiled on Windows.
- Keep `crates/core` free of UI/terminal dependencies; the CLI depends on core, never the reverse.
- Do not edit `release.yml`, `PKGBUILD`, `Makefile` or versions unless the spec says so.
- Do not run the TUI, traceroute or speed test yourself; ask the user to.
- Never commit without the user's approval. Commit messages in English.
