# Test cleanup ledger

Keep removals tied to a coverage owner. A passing help or fixture test does not
prove a Gateway lifecycle.

| Scope | Decision | Coverage retained | Validation |
| --- | --- | --- | --- |
| `tests/launcher_help_tests.rs` | Remove 111 exact-prose or redundant keyword assertions; preserve command syntax, options, help-entry equivalence, version and error routing. | Behavior remains in the runtime, upgrade, environment, service and CLI integration suites. Help tests cover help output only. | Linux formatting, release-policy checks, all-target compilation, Rust suites and isolated install/version smoke passed; CI matrix pending. |

Remaining audit: source unit tests, Python packaging/release tests, lifecycle
fixtures and Rust integration suites. Earlier cleanup commits on main already
consolidated shell/download tests and shared package/supervisor fixtures. Review
remaining cases against current coverage before deleting them.

Initial inventory at `52a2c76`: 2,144 Rust test annotations across integration
and source tests, plus six Python test files. A text scan found no identical
Rust test bodies; this is candidate discovery, not proof of unique behavior.
The three ignored Rust cases require native Windows downloads or a privileged
macOS reader and remain intentional.

Examples of retained behavior owners: `env_clone_accepts_an_explicit_target_sandbox_origin`,
`env_import_resets_or_replaces_a_copied_public_sandbox_origin`,
`runtime_remove_rejects_a_runtime_bound_to_an_environment`, and
`bound_official_runtime_is_reused_but_not_refreshed`.

Remote proof: Rust 1.98.1, Node.js 24.19.0 and npm 11.19.1. The native
companion control failed with the worker image’s npm 11.17.0; it passed with
npm 11.19.1, followed by the full runtime-command suite and the unrun remainder.
The unused-variable correction passed formatting, all-target compilation and
the focused help suite. CI owns the declared Node and platform matrix.
