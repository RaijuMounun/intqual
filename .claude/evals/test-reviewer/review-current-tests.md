# Case: review-current-tests

Mode `review`. Known-answer case on the tests as of commit `58f1154`. If the tests change, update the
rubric before running.

## Brief (send as the agent's prompt)

<objective>
Review all existing tests in the intqual workspace (mode: review) and report their quality, their gaps,
and whether they test behavior or implementation details.
</objective>
<context>
Test modules: crates/core/src/network/icmp/packet.rs, crates/core/src/network/icmp/windows.rs,
crates/core/src/network/mod.rs, crates/core/src/engine/mock.rs, crates/core/src/engine/core_engine.rs,
crates/cli/src/ui/widgets/traceroute.rs. Project rules: CLAUDE.md. Testing plan: docs/assessment.md §6.
</context>
<done_when>
Every test module above has been read and judged, and the untested modules named in the testing plan
are covered as gaps.
</done_when>
<output_format>
Your standard output contract.
</output_format>

## Rubric

PASS only if all of these hold:
1. It flags at least 3 of these known issues:
   - `ping_loopback_without_admin` asserts only `rtt >= 0.0` (weak; true for almost any value).
   - `ipv6_targets_are_rejected` asserts `is_err()` without the error variant.
   - `mock.rs` and `core_engine.rs` tests synchronise with real time (`Duration` timeouts, 5 ms interval)
     instead of paused time.
   - `running_screen_has_no_error_line` uses a negative `!contains("Error")` check that passes on a
     blank screen.
   - No tests for `NetworkStats::update`, `AppState::handle_event`, `TcpProbe`, `TracerouteHop` serde.
2. Each finding has a file:line reference.
3. Every proposed test has a level (unit / loopback integration / manual `#[ignore]`).
4. The transcript has no Edit or Write call, and Bash ran only `cargo test`, `cargo llvm-cov`, or `git`.

FAIL if it proposes a test that needs admin or the internet without `#[ignore]`.
