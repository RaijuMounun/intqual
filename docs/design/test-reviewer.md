# Design: `test-reviewer` agent for intqual

Status: **approved 2026-11-04** · Date: 2026-11-04 · Built with `agent-architect` (Create flow).

## Job

Reviews intqual's tests. It is the "test review" stage of the brainstorm → spec → tests → code → review
loop, and it runs in three modes, chosen by the brief:

| Mode | Input | Output |
| :- | :- | :- |
| `review` | files or a diff with tests | Findings on quality, gaps, and behavior vs. implementation detail |
| `gaps` | code with no tests | The tests it should have, the level of each, and how to make the code testable |
| `plan` | a spec in `docs/specs/*.md`, before code exists | A test plan that maps each requirement and "done when" line to tests |

It never writes or edits files. Writing tests is the test writer's job (a later stage of the loop);
a reviewer that can edit would "fix" what it should report.

## Shape (smallest that does the job)

Project-local, as the user chose. No plugin.

```
.claude/
├── agents/test-reviewer.md          # the worker: method, boundaries, output contract
└── skills/test-review/
    ├── SKILL.md                     # entry skill: picks the mode, writes the brief, dispatches, relays
    └── references/
        ├── checklist.md             # review rules (distilled research, rated)
        └── rust-testing.md          # intqual-specific techniques: fakes, paused time, loopback, tools
└── evals/test-reviewer/             # known-answer cases + HISTORY.md (see Eval plan)
```

- **Why an entry skill.** It turns "testleri incele" or "bu spec için test planı çıkar" into a full
  brief (mode, files, spec path), relays `needs_input` questions, and presents the result in Turkish.
  The agent stays generic over modes.
- **Why references are read, not preloaded.** `skills:` preloading would also load the entry skill's
  dispatch instructions into the agent. The agent `Read`s the two reference files at start instead.
- **Project rules** come from `CLAUDE.md` and `docs/assessment.md` at runtime, never copied into the
  prompt, so they stay current.
- **Promotion later.** If a second project needs it, the method (agent + checklist) moves to an
  ai-playbook `test-review` bundle and `rust-testing.md` stays here. Recorded in `docs/decisions.md`.

## Agent

- `model: sonnet`, `tools: Read, Grep, Glob, Bash`.
- Bash is for `cargo test --workspace` (does it pass, which tests exist), `cargo llvm-cov` only when it
  is installed, and `git diff`/`git log`. The prompt forbids other commands; nothing enforces it, so the
  trial run checks the transcript.
- It does not run `cargo mutants` (minutes to hours); it recommends `cargo mutants --in-diff` instead.
- Output contract (fixed sections): `status: done | needs_input`, Summary (3 lines), Findings table
  (severity, file:line, what, why it matters, suggested test or change), Gaps / Test plan (test name,
  level: unit / loopback integration / manual `#[ignore]`, what it proves), Testability changes,
  Not checked (e.g. `cfg(unix)` code it could not compile). Under ~800 words.
- `needs_input`: 1–4 questions with options, recommendation first, only when the brief leaves scope
  open (for example, a spec with no "done when" section).

## What it checks (from research, rated for intqual)

Adopt:
1. Behavior through the public API, outcome (state) asserted, not internal calls; one behavior per test;
   the name says action + outcome. ([SWE at Google ch. 12](https://abseil.io/resources/swe-book/html/ch12.html))
2. Smells: logic in tests, irrelevant setup, weak assertions (`is_err()` without the variant,
   `rtt >= 0.0`), failure messages without expected/actual. "Would a mutation of this line fail a test?"
3. Determinism: no real `sleep` for synchronisation, no wall-clock thresholds, no fixed ports, no internet,
   no admin; loopback `127.0.0.1:0` only. Anything else needs `#[ignore]` with a reason (project rule).
4. Fakes over mocks: hand-written `IcmpProvider` fakes that can fail (timeout, unreachable, malformed)
   and record inputs; `mockall` is Assess only (its `#[concretize]` clashes with the no-`dyn` rule).
   ([mockall 0.15](https://docs.rs/mockall/latest/mockall/))
5. Tokio: `#[tokio::test(start_paused = true)]` + `advance` for intervals, timeouts and cancellation;
   never with real sockets or `spawn_blocking` (auto-advance stops there). Shutdown through
   `CancellationToken`, then `rx.recv() == None`; `try_send` `Full` with a capacity-1 undrained channel.
   ([tokio::time::pause](https://docs.rs/tokio/latest/tokio/time/fn.pause.html), 1.54)
6. FFI: pure mapping functions extracted and table-tested (like `map_status` in `icmp/windows.rs`).
7. `cargo-llvm-cov` to find untested code, never as a target. ([cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov);
   skeptic: [Fowler, TestCoverage](https://martinfowler.com/bliki/TestCoverage.html))

Trial (recommended, not required):
- `proptest` for `packet.rs`: decode(encode(x)) == x, decode never panics on arbitrary bytes.
  ([proptest book](https://proptest-rs.github.io/proptest/intro.html))
- `insta` snapshots of widgets with `TestBackend` at a fixed size; deterministic input only; it cannot
  see colour or key handling. ([ratatui snapshot recipe](https://ratatui.rs/recipes/testing/snapshots/))
- `cargo mutants --in-diff` per change. ([mutants.rs](https://mutants.rs/in-diff.html))
- `wiremock` for the speed test once the base URL is injectable. ([wiremock 0.6.5](https://docs.rs/wiremock/latest/wiremock/))

Assess: `cargo-fuzz`, `mockall`.

## Eval plan

`claude plugin eval` only runs plugins, so these cases are run by hand: dispatch the agent with the
case's brief, then grade the reply against the case's rubric. Each case is a known-answer check: the
rubric lists facts about the current code that a good review must find.

1. `review-current-tests` — mode `review` on all current tests. PASS only if it (a) flags at least 3 of:
   weak `rtt >= 0.0` and `is_err()` assertions in `icmp/windows.rs`, real-time waits in `mock.rs` and
   `core_engine.rs` tests, the negative `!contains("Error")` check in `widgets/traceroute.rs`, no tests
   for `NetworkStats`/`AppState::handle_event`/`TcpProbe`/`TracerouteHop` serde; (b) gives file:line for
   each finding; (c) makes no Edit/Write call and runs only allowed commands.
2. `gaps-ui-state` — mode `gaps` on `crates/cli/src/ui/mod.rs`. PASS only if every proposed test has a
   level, none needs admin or internet without `#[ignore]`, and it names a testability change where one
   is needed.
3. `plan-from-spec` — mode `plan` on `docs/specs/windows-traceroute.md`. PASS only if every "Tests" and
   "Done when" line maps to a test or to a manual step, manual ones are marked, and it writes no code.
4. `needs-input-unclear-scope` — a brief that names a spec file that does not exist. PASS only if it
   returns `status: needs_input` with options instead of guessing.

Done for this build: case 1 is the trial run the user reads. Cases 2–4 run afterwards on request
(about one sonnet subagent run each).

## Open questions

- None blocking. Later: does the test writer (next loop stage) loop with this agent automatically, and
  with how many rounds? That belongs to the per-project loop design.
