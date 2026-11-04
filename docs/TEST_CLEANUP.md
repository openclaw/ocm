# Test cleanup ledger

Keep removals tied to a coverage owner. Help and fixture tests do not prove a
Gateway lifecycle. Remove a product mapping only when its owner is obsolete.

| Scope | Decision | Coverage retained | Proof |
| --- | --- | --- | --- |
| Help assertions | Remove 111 exact-prose or redundant keyword assertions. | Command syntax, options, help-entry equivalence, version and error routing; behavior owners in the runtime, upgrade, environment, service and CLI suites. | Remote baseline and full CI passed in [PR #989](https://github.com/openclaw/ocm/pull/989). |
| [Completed upgrade campaign](https://github.com/openclaw/ocm/pull/992) | Remove four CI steps gated exclusively to merged [PR #568](https://github.com/openclaw/ocm/pull/568) and their sole 401-line script. | Regular Rust upgrade/self-update suites and native Gateway lifecycle checks. | Remote formatting, 41 release-policy tests, all-target compilation and retained-workflow inspection passed; unchanged Rust/install proof reused from #989. CI tracked in the linked PR. |
| [Runtime/release install aliases](https://github.com/openclaw/ocm/pull/994) | Share the copied managed-Node fallback scenario and remove its duplicate fixture import. | Both CLI entry points, quiet stdout, successful install and each command's registry-request limit. | Remote formatting, all-target compilation, 90 runtime and 7 release tests passed. CI tracked in the linked PR. |
| Tar archive fixtures | Share three identical tar-entry builders. | The same headers, modes and archive payloads in runtime, upgrade and common fixtures. | Remote formatting, 41 release-policy tests, all-target compilation, Rust suites and isolated install/version smoke passed. CI tracked on this PR. |
| [Copied executable fixtures](https://github.com/openclaw/ocm/pull/996) | Share the existing bounded exec-admission retry with the common binary runner; remove two local copies. | Only `ExecutableFileBusy` before a child starts is retried, at most five attempts. Process exit failures and all other errors remain failures. | Full ordered remote baseline passed with the shared helper, including the self-update and daemon suites and isolated install/version smoke. CI tracked in the linked PR. |
| [Native shutdown proof](https://github.com/openclaw/ocm/pull/995) | Wait up to 30 seconds after an accepted stop; reject renewed running intent, status failures and deadline expiry. | Native child-PID, installed-state, port and cleanup assertions stay intact. | Remote formatting, 41 release-policy tests, 5 shutdown regressions, Python syntax and all-target compilation passed; native CI tracked in the linked PR. |
| [Native npm prerequisite](https://github.com/openclaw/ocm/pull/993) | Document npm 11.19.1 for the Rust native companion tests. | Native npm control and installed companion dependency graph checks. | Worker npm 11.17.0 failed before OCM ran; isolated npm 11.19.1 passed. Remote Markdown check passed. |
| Background-resume PTY diagnostics | Capture child stderr without pipe backpressure; report its bounded tail, wait phase, child exit and terminal foreground group on failure. | Existing background-stop, foreground-input, watch-lease and cleanup assertions and deadlines. | Remote formatting, 41 release-policy tests, 5 shutdown tests, all-target compilation, Rust suites and isolated install/version smoke passed. Five injected failure/success controls passed, including 128 KiB of child stderr with a 16 KiB printed tail. CI tracked in the follow-up PR. |

## Audit coverage and retained tests

Inventory at `52a2c76`: 2,144 Rust test annotations across source and integration
tests, plus six Python test files. Review included body-duplication scans,
fixture/helper reference scans, CLI/workflow mappings and recent CI failures.
Candidate scans guide review; they do not prove every possible case is unique.

- No exact Rust test-body copies remained after normalizing fixture labels.
  Python AST review found no exact copies among 63 test methods. Inherited
  release-policy cases intentionally exercise renamed packages.
- Cross-file similarity found the install-alias copy above. Keep the similar
  runtime/launcher registry-lock cases and runtime/release reuse cases: their
  resource and binding owners differ.
- Assertion-free candidates delegate to helpers with assertions or bounded
  observation of real output. Keep them. Reference scans found no additional
  unused public support helpers or fixtures; Windows scripts and the Python
  provider module have workflow/import owners outside Rust test sources.
- Keep all three ignored Rust cases: native Windows distribution download,
  Windows host compatibility, and the privileged macOS configuration reader.
  CI explicitly runs them in the required native environments.
- Keep the isolated self-update transaction that builds two release binaries.
  Its runtime cost buys an actual replacement/rollback lifecycle.

Examples of distinct behavior owners: explicit clone/import sandbox origins,
removal of bound runtimes, and reuse of a bound official release. Fake service
managers test deterministic failures; native Gateway proofs test real service
lifecycle and provider transport. Neither substitutes for the other.

## Follow-up observations

Two historical macOS retries failed different assertions: provider-message
matching in [run 41445179922](https://github.com/openclaw/ocm/actions/runs/41445179922)
and stopped-service state in
[run 41479799376](https://github.com/openclaw/ocm/actions/runs/41479799376).
The first passed on retry; preserve its strict message matching and capture the
exact provider request if it recurs. The stop assertion failed on both attempts:
`wait_for_action_summary` deliberately returns after three seconds with a warning
when shutdown is still in progress. The native proof must poll for completion,
not equate accepted stop with immediate child exit. The bounded-wait fix above
preserves the final lifecycle assertions. The current native macOS proof passed
in #989; these failures do not justify deleting lifecycle coverage.

The full worker run also exposed transient Linux `ExecutableFileBusy` while
launching a copied binary, before OCM ran. The shared retry above follows the
existing daemon fixture policy; it never retries a process exit.

The unchanged background-resume PTY test timed out waiting for foreground input
in [run 41826860341](https://github.com/openclaw/ocm/actions/runs/41826860341).
A single targeted macOS retry passed. Its failure previously hid child stderr;
the diagnostics above preserve the original exception and print at most 16 KiB
of child output. This records evidence for a recurrence without claiming a root
cause or weakening the lifecycle test.

During the diagnostics follow-up, the unchanged supervisor publication-retry
fixture failed when its selected port family was already occupied. The other
601 source tests passed. That exact fixture passed alone, followed by all unrun
integration, binary and documentation tests and the isolated install. Keep this
port-contention observation as a separate fixture-isolation follow-up.

Remote baseline uses Rust 1.98.1, Node.js 24.19.0 and npm 11.19.1. CI owns the
declared Rust/Node and native platform matrix. No release is part of this audit.
