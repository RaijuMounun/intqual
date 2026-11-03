# Test review checklist

Distilled from web research on 2026-11-04 (sources at the end). Ratings are for intqual:
**Adopt** = expected of every test, a miss is a finding; **Trial** = recommend where it fits;
**Assess** = mention only if asked.

## What a good test does (Adopt)

1. **Tests behavior through the public API.** It calls what callers call and asserts the outcome
   (returned value, emitted event, rendered text, final state), not which internal function ran.
   A test that breaks on a refactor that keeps behavior is testing implementation detail.
2. **One behavior per test.** One "when", one "then". A name that needs "and" is two tests. Table tests
   over one function with many inputs are fine.
3. **The name states action and outcome**, e.g. `decode_rejects_truncated_time_exceeded`.
4. **Assertions that a bug would break.** Weak forms to flag:
   - `is_err()` / `is_ok()` where the error variant or value matters (`matches!(e, ProbeError::X)`).
   - Bounds that almost any value satisfies (`rtt >= 0.0`).
   - Negative checks alone (`!contains("Error")`) that also pass on an empty or blank result; pair them
     with a positive check.
   - Asserting only that something was called.
5. **Failure messages carry expected, actual and inputs** (`assert_eq!` or a message with the
   parameters), so a red CI run is readable without a debugger.
6. **No logic in tests.** Loops and conditionals that compute the expected value hide bugs; a loop over
   fixed cases is fine. Setup shows only what matters to this test (DAMP over DRY).
7. **Deterministic.** No real `sleep` for synchronisation, no wall-clock thresholds, no fixed ports, no
   shared global state, no internet, no admin. Loopback `127.0.0.1:0` is fine. Anything else is
   `#[ignore]` with a comment saying why (project rule).
8. **Test helpers are code.** A helper that builds packets or state must be simple enough to trust;
   a bug there makes every test that uses it lie.

## Coverage and test strength

- **Adopt.** Use coverage (`cargo llvm-cov`) to find code that no test runs. Do not propose a coverage
  target: coverage says what ran, not what was checked.
- **Trial.** Mutation testing (`cargo mutants --in-diff`) answers "would a test fail if this line were
  wrong?" for changed code. Recommend it; don't run it.

## Doubles

- **Adopt.** Prefer real objects, then hand-written fakes behind a trait (`IcmpProvider`). A fake should
  be scriptable for failures (timeout, unreachable, malformed reply) and record its inputs so the test
  asserts state.
- **Assess.** `mockall`: works with async fn in traits, but interaction-heavy tests are brittle and its
  `#[concretize]` path uses trait objects, which the project avoids.

## Severity guide

- **High:** a test that cannot fail, or passes while the behavior it names is broken; a test that needs
  admin or the internet without `#[ignore]`; untested code that decides what the user sees as loss,
  latency or an error.
- **Medium:** weak assertion, real-time wait that may flake, missing failure case for a tested function.
- **Low:** naming, message quality, duplicated setup.

## Sources

- Software Engineering at Google, ch. 12 "Unit Testing": https://abseil.io/resources/swe-book/html/ch12.html
- Martin Fowler, "TestCoverage" (2012-04-17): https://martinfowler.com/bliki/TestCoverage.html
- cargo-mutants, `--in-diff` and cautions: https://mutants.rs/in-diff.html, https://mutants.rs/cautions.html
- cargo-llvm-cov README (Windows MSVC supported, status 2025-12-30): https://github.com/taiki-e/cargo-llvm-cov
- mockall 0.15.0 docs: https://docs.rs/mockall/latest/mockall/
