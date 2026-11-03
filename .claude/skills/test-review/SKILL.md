---
name: test-review
description: >-
  Reviews intqual's tests through the test-reviewer agent: judges existing tests, finds untested code
  and says how to test it, or turns a feature spec in docs/specs/ into a test plan before coding. Use
  when the user asks to review tests, check test quality or coverage gaps, or plan tests for a spec,
  in English or Turkish ("review the tests", "what tests does this need", "test plan for this spec",
  "testleri incele", "testler yeterli mi", "bu koda hangi testler lazım", "bu spec için test planı çıkar").
---

# Test review

Runs the `test-reviewer` agent on one scope and presents its report to the user in Turkish. The agent
holds the method; this skill decides the scope, writes the brief, and relays questions.

## 1. Pick the mode and scope

| User asks about | Mode | Scope to put in the brief |
| :- | :- | :- |
| existing tests, a diff, "are these tests good" | `review` | the test files or the diff (`git diff <base>`) |
| code with no tests, "what tests does X need" | `gaps` | the source files |
| a spec, a planned feature | `plan` | the spec path under `docs/specs/` |

If the scope is unclear (no file, spec or diff can be inferred), ask the user one question with options
before dispatching. Default to the smallest scope that answers the request: a run over the whole
workspace costs more and buries the findings.

## 2. Dispatch one agent

Write the brief with the `prompt-craft` template. Keep it short; the agent already knows the method:

```xml
<objective>
<Mode> for <scope>: <what the user wants to decide>.
</objective>
<context>
Files or spec: <paths>. Why now: <the loop stage, e.g. "spec approved, tests not written yet">.
Anything the user said that narrows it.
</context>
<done_when>
review: every listed test judged. gaps: every public function or state transition has a proposed test
or a reason it needs none. plan: every requirement and "Done when" line maps to a test or a manual step.
</done_when>
<output_format>
Your standard output contract.
</output_format>
```

One agent per request. Tell the user before running on more than one crate at once.

## 3. Relay questions

If the agent returns `status: needs_input`, ask its questions with a question card (options as given,
recommendation first), then continue the same agent with `SendMessage` and the answers. Don't start a
new agent.

## 4. Present the result

In Turkish, plain language, technical terms explained once:

- the summary and the high-severity findings first, with file:line links;
- the tests to add, grouped by level;
- what was not checked.

Keep the agent's tables; translate the prose around them. Don't add findings of your own without
saying they are yours. Writing or changing tests is a separate step the user approves.
