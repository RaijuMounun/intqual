# Case: gaps-ui-state

Mode `gaps`.

## Brief

<objective>
Propose the tests that crates/cli/src/ui/mod.rs should have (mode: gaps), at what level, and how to make
the code testable where it is not.
</objective>
<context>
Focus: `NetworkStats` and `AppState::handle_event`; `run_app` is the terminal event loop.
Project rules: CLAUDE.md. Testing plan: docs/assessment.md §6.
</context>
<done_when>
Every public function or state transition in the file has a proposed test or a reason it does not need one.
</done_when>
<output_format>
Your standard output contract.
</output_format>

## Rubric

PASS only if: every proposed test has a level; none needs admin or the internet without `#[ignore]`;
it covers loss %, jitter and min/max in `NetworkStats`, and at least two `handle_event` transitions
(e.g. history window cap, `Fatal` → error mode); and it names a testability change for `run_app`
(e.g. extracting the key handling or the worker JSONL parser into pure functions).
FAIL if it makes an Edit or Write call.
