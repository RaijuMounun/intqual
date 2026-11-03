# Spec: Working traceroute (and ping) on Windows

Status: **draft, awaiting approval** · Date: 2026-11-03 · Scope: Windows only (Unix paths unchanged).

## Problem

Pressing `t` on Windows shows an empty "Traceroute (Running...)" table and never an error,
even from an elevated shell (reported by the user, default target `google.com`).

Root causes (code at commit `a2941a4`):

1. The UI always launches traceroute through `sudo` (`crates/cli/src/ui/mod.rs`, the `KeyCode::Char('t')`
   branch: `sudo -v`, then `sudo -E <exe> --worker-mode traceroute`). Windows 11 ships
   `C:\Windows\System32\sudo.exe`, but it does not support `-v`/`-E` like Unix sudo and, in its
   default mode, runs the command in a new window, so the piped stdout gets no JSON lines.
2. Every failure in that path becomes `TelemetryEvent::TracerouteError` → `AppState::last_error`,
   but `TracerouteWidget` never renders `last_error` (only `bandwidth.rs` does). Errors are invisible.
3. Even without sudo, the Windows provider uses a raw ICMP socket (`icmp/windows.rs`). Receiving
   ICMP *Time Exceeded* from remote routers on a raw socket is subject to Windows Firewall and
   admin rules; it worked only for a loopback target in our test.

## Goal

On Windows, from a normal (non-admin) shell, `t` shows hops to the target with RTT and hostnames,
and any failure is shown on screen.

## Approach (standard practice)

Use the Windows ICMP Helper API — `IcmpCreateFile` / `IcmpSendEcho` / `IcmpCloseHandle` from
`iphlpapi.dll` — which is what `ping.exe` and `tracert.exe` use. It needs no admin, is not
blocked by the firewall the way raw sockets are, and supports a TTL via `IP_OPTION_INFORMATION`.

1. **New Windows provider** `crates/core/src/network/icmp/windows.rs` rewritten on the ICMP API,
   via the `windows-sys` crate (`0.61`, already in `Cargo.lock`), as a
   `[target.'cfg(windows)'.dependencies]` entry of `intqual-core`.
   - `ping`: `IcmpSendEcho` with the timeout; `IP_SUCCESS` → RTT, `IP_REQ_TIMED_OUT` →
     `ProbeError::IcmpTimeout`, others → `ProbeError::IcmpError { .. }`.
   - `send_with_ttl`: same call with `Ttl` set; map `IP_SUCCESS` → `IcmpResponse::EchoReply`,
     `IP_TTL_EXPIRED_TRANSIT` → `TimeExceeded`, `IP_DEST_*_UNREACHABLE` → `DestinationUnreachable`,
     responder IP from `ICMP_ECHO_REPLY.Address`.
   - The call blocks, so it runs in `tokio::task::spawn_blocking`. Handle opened and closed per call
     (no shared state), closed on every path. Each `unsafe` FFI block gets a `// SAFETY:` comment.
   - This also fixes the per-recv timeout bug (assessment §4.3), since the API enforces one timeout.
   - `IcmpEchoRequest`/checksum code stays; it is still used on Unix and by tests.
2. **In-process traceroute on Windows.** Under `#[cfg(windows)]`, the `t` key sends
   `EngineCommand::StartTraceroute` instead of spawning the sudo worker. In `CoreEngine`:
   - store the traceroute `CancellationToken` so `Resume`/`Stop` cancel it (today it is dropped);
   - report probe errors as `TelemetryEvent::TracerouteError` (not `Fatal`, which replaces the
     screen with the "sudo required" error page);
   - for each hop with an IP, spawn a reverse-DNS lookup and send `TelemetryEvent::DnsResolved`
     (same behavior as the worker path; `dns-lookup` is already a core dependency).
   The Unix sudo-worker path is unchanged.
3. **Show errors.** `TracerouteWidget` renders `app.last_error` (red line under the table title)
   on all platforms.
4. **Small cleanup in scope:** the `t` branch in `ui/mod.rs` moves into its own function
   (`start_traceroute_worker`) so the `cfg` split stays readable and rustfmt can format it.

## Out of scope

IPv6 (assessment §4.4), Unix behavior, the README/winget wording (separate change once this works).

## Tests

- Unit (no network): status-code → `IcmpResponse`/`ProbeError` mapping is a pure function and gets
  table tests. `TracerouteWidget` shows `last_error` (ratatui `TestBackend`).
- Integration, no admin, loopback only (Windows): `ping` and `send_with_ttl(ttl=1)` to `127.0.0.1`
  return `EchoReply` with RTT ≥ 0. Gated `#[cfg(windows)]`.
- Manual (user, Windows, normal shell): `cargo run -p intqual`, press `t`; expect several hops to
  `google.com` ending in the destination, then `Esc`/`Enter` returns to ping and ping resumes.

## Done when

The manual test passes in a non-admin shell; build, clippy (`-D warnings`) and tests pass; CI green.
