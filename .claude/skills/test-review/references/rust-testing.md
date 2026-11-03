# Rust testing techniques for intqual

How to test each kind of intqual code without admin or the internet. Distilled from research on
2026-11-04; ratings as in `checklist.md`.

## Levels

| Level | Where | Runs in `cargo test` | Example |
| :- | :- | :- | :- |
| Unit | in-file `#[cfg(test)] mod tests` | yes | `packet.rs` encode/decode, `map_status` |
| Loopback integration | in-file or `crates/*/tests/`, may be `#[cfg(windows)]` | yes | ICMP Helper API to `127.0.0.1`, `TcpProbe` against a local listener |
| Manual | `#[ignore]` + comment with the reason | only with `-- --ignored` | traceroute to a remote host, Cloudflare speed test |

## Async and time (Tokio)

- **Adopt.** `#[tokio::test]` uses a current-thread runtime. `#[tokio::test(start_paused = true)]` (or
  `tokio::time::pause()`) freezes Tokio's clock; when the runtime is idle it jumps to the next timer, so
  intervals and timeouts run instantly and deterministically. Needs the `test-util` feature of tokio
  as a dev-dependency.
- Pitfalls: only `tokio::time::Instant` is frozen, `std::time::Instant` keeps running; auto-advance stops
  while a `spawn_blocking` task runs; paused time with real sockets or `IcmpSendEcho` timeouts gives
  misleading results. So: paused time with a fake provider, real time only for loopback I/O tests.
- **Cancellation.** End tasks with `CancellationToken::cancel()`, then await the join handle or check that
  `rx.recv().await` returns `None` once the sender drops. Never `abort`.
- **Back-pressure.** Test the `try_send` `Full` path with a capacity-1 channel nobody drains: the producer
  must keep going and drop the frame. Test `Closed` by dropping the receiver: the producer must stop.
- Real-time `tokio::time::timeout` around loopback I/O is acceptable as a safety net; using it to wait
  for a timer the code itself sets is a flake risk (use paused time).

## ICMP and other OS APIs

- **Adopt.** Byte-level logic through `packet.rs` buffers (project rule).
- **Adopt.** FFI status codes go through a pure mapping function with a table test (see `map_status` in
  `icmp/windows.rs`). New FFI code without such a function is a testability finding.
- **Adopt.** Probes that construct their provider inside (`DefaultIcmpProvider::new`) cannot take a fake;
  making them generic over `P: IcmpProvider` is the unblocking change (assessment §5 item 6).
- **Trial.** `proptest` properties for `packet.rs`: `decode(encode(x))` returns the same id and sequence,
  a packet including its checksum sums to zero, `decode` never panics on arbitrary bytes.
- **Assess.** `cargo-fuzz`: the never-panics proptest covers most of its value at this size.

## TCP and HTTP

- **Adopt.** `TcpListener::bind("127.0.0.1:0")` then `local_addr()` for the port. Refused port: bind, read
  the port, drop the listener, connect. Never a hard-coded port.
- **Trial.** `wiremock` (0.6.5) for the speed test, once the base URL is a parameter instead of the fixed
  Cloudflare host. Until then, extract the Mbps/progress maths into pure functions and unit-test those.

## TUI (ratatui)

- **Adopt.** Widgets are pure render functions: render a fixed `AppState` into `TestBackend` and assert on
  the buffer text. Assert the positive content, not only its absence.
- **Trial.** `insta` snapshots (`assert_snapshot!(terminal.backend())`) at a fixed size with deterministic
  state (no clock, no random). Limits: no colour or style checks, no key handling, and snapshots are easy
  to accept without reading in `cargo insta review`.
- `AppState::handle_event` and `NetworkStats` are plain state machines: unit tests with scripted event
  sequences, no terminal needed.

## Platform notes

- This machine is Windows 11: `cfg(unix)` code (`icmp/unix.rs`, `icmp/unix_raw.rs`, the sudo worker) is
  not compiled here. CI compiles and tests it on Linux and macOS. Proposed Unix tests are fine; say they
  were not compiled locally.
- `cargo-llvm-cov` works on `x86_64-pc-windows-msvc`; Unix files show as absent locally.

## Sources

- tokio::time::pause (tokio 1.54): https://docs.rs/tokio/latest/tokio/time/fn.pause.html
- ratatui snapshot testing recipe: https://ratatui.rs/recipes/testing/snapshots/
- Proptest book: https://proptest-rs.github.io/proptest/intro.html
- wiremock 0.6.5: https://docs.rs/wiremock/latest/wiremock/
- cargo-fuzz book: https://rust-fuzz.github.io/book/cargo-fuzz.html
- cargo-llvm-cov: https://github.com/taiki-e/cargo-llvm-cov
