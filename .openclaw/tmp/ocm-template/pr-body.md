<!--
Optional linked context:
Add a visible `Closes #<issue-number>` or `Related: #<issue-number>` line
below this comment.

Write for someone who uses OCM but has not read the code. The title and
opening problem statement should make sense without the diff or linked issue.

Required PR title:
type: <what changes for the person affected>
Types: feat, fix, improve, refactor, docs, chore.
Use a parenthesized scope only when it helps that reader.

Name the affected task and the symptom or outcome, not the implementation:
Good: fix: show the correct connection URL for TLS-enabled environments
Bad: fix(service): normalize the URL scheme in status output

For internal-only work, name the actual developer or operator workflow affected.

**MUST:** Keep **Allow edits from maintainers** enabled for this PR so maintainers
can help update the branch when needed.
-->

## What Problem This Solves

<!--
Describe the concrete user, product, or operational problem.
In 1–3 short, plain-language sentences, explain what someone is trying to do,
what goes wrong or is missing, and why it matters. For a fix, include the trigger
when known. For other changes, describe the need without inventing a bug.

Good: "Operators checking a TLS-enabled environment see an HTTP URL, so
connection attempts use the wrong protocol."
Bad: "Fixes incorrect scheme selection in the service status renderer."

Describe the experienced problem, not just the code change. Save function names,
missing checks, and the root-cause walkthrough for the explanation below.
-->

OCM pull requests can name implementation details without explaining the operator task or what validation actually demonstrated, making changes harder to assess without reading the code.

## User Impact

<!--
"User impact: <what users, operators, or developers can now do or expect>."
Lead with the concrete outcome in plain language, usually one sentence.
For internal-only changes, say there is no user-visible change; do not invent a benefit.
Keep important risks, breaking changes, migrations, and required user actions visible here.
-->

Contributors get OCM-specific examples for explaining the problem and outcome, while reviewers see the observed result before command lists. There is no change to OCM behavior, required checks, security policy, or operator actions.

## Why This Change Was Made

<!--
Briefly explain how the change addresses the problem without repeating the impact.
Keep the body short. Leave file lists, internal acronyms, and root-cause walkthroughs
in the diff or optional <details>; include technical detail only when it explains
behavior or a material tradeoff. Do not hide risks or required actions in <details>.
-->

Refine the existing author-facing comments instead of adding another template or review gate. The four headings, maintainer-edit requirement, exact problem prompt, and existing impact/rationale guidance remain unchanged. TLS connection-URL examples use OCM’s documented service-status workflow.

## Evidence

<!--
Show the most useful proof that this change works. Screenshots, screencasts,
terminal output, focused tests, CI results, live observations, redacted logs,
and artifact links are all useful. Include before/after evidence for visual
changes when it clarifies the result.

Start with the behavior checked and the result in plain language: for example,
what the CLI reported or what happened during an isolated environment lifecycle.
Reviewers will inspect the code, tests, and CI; do not make them decode command
lists to learn what was verified. Put exact commands, commit hashes, and long
output in links or optional <details>. Keep the useful evidence summary and
meaningful gaps visible. Use the existing checks in CONTRIBUTING.md; this
section does not add a new test or require changing a real environment.
-->

The candidate preserves every byte outside HTML comments, including all four headings, while making the title/problem examples and evidence instructions reader-first. Remote validation passed and independent read-only review found no actionable issues.

- The TLS example agrees with the [service-status documentation](https://github.com/openclaw/ocm/blob/6879db78f36bd86adfe2f6dec860d2b8520aa8d2/docs/USAGE.md#service-management); repository template consumers and CI guidance were reviewed.
- Followed the existing Markdown-prose-only path in `CONTRIBUTING.md`: remote whitespace check plus source/CI parity. No shell recipe or executable input changes; Rust, packaging and lifecycle suites were not run locally or on the disposable worker. Hosted CI remains separate.
- This description is authored from the candidate template. Automatic browser prefill after landing is not claimed.

<details>
<summary>Remote verification and source identity</summary>

An owned AWS Linux worker received the exact Git bundle and patch.

```text
Base: 6879db78f36bd86adfe2f6dec860d2b8520aa8d2
Candidate: bdb8366665999c644abc5023beb5466908ee0c2d
Patch blob: 9b018aaa4fd98ac2911c62ee88be6655afba1863
Cargo.lock blob (unchanged): e65ce614f7367356f9b4bd8127b316dc2197fcff
Template blob: 7ed808961706510c127820718f5818d62bfec303
Linux 7.0.0-1014-aws x86_64; Git 2.53.0; Python 3.14.4
git diff --check HEAD: PASS
Structural assertions: PASS
```

Assertions compare all noncomment bytes, heading order, the exact problem sentence, maintainer-edit instructions, unchanged impact/rationale guidance and checklist parity (none in either version); only `.github/pull_request_template.md` changes.

</details>

Requested by and co-authored with @hannesrudolph.

## Work sessions

Linked for team reference; these sessions aren’t public.

- [Original discussion with @hannesrudolph](https://team.openclaw.ai/chat/roboclaw/~key/dashboard/3b8142ee-a68a-4501-b00a-6e45bfe88c0e)
- [Implementation and verification](https://team.openclaw.ai/chat/roboclaw/~key/dashboard/f56d3fc6-ef96-4731-b5d2-08990b309e5f)
