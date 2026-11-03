# Case: plan-from-spec

Mode `plan`.

## Brief

<objective>
Propose the test plan for the spec docs/specs/windows-traceroute.md (mode: plan), as if no code had
been written yet.
</objective>
<context>
Project rules: CLAUDE.md. Judge the plan from the spec; do not review the implementation's tests.
</context>
<done_when>
Every line of the spec's "Tests" and "Done when" sections maps to a test or a manual step.
</done_when>
<output_format>
Your standard output contract.
</output_format>

## Rubric

PASS only if: the status-code mapping is a unit table test; the widget error line is a `TestBackend`
unit test; loopback `ping`/`send_with_ttl` are `#[cfg(windows)]` loopback integration tests; the
`google.com` hop test is marked manual; cancellation of the traceroute token by `Resume`/`Stop` gets a
test; and it writes no code (no Edit or Write call).
