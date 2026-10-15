# PR3 independent review

## Done
Reviewed main..HEAD core snapshot, provider isolation and regression-test changes, plus both daily-review specialist handoffs. Ran a real CLI reproduction with a local deterministic provider; no model invocation or implementation edits.

## Found
P2 regression in crates/daf-review/src/provider.rs executable selection/current_dir: a bare provider executable found through a relative PATH entry is fingerprinted in the caller directory, but launched after moving to the temporary directory, where that PATH entry resolves differently. With PATH=bin:$PATH and --provider-executable review-provider, the valid executable in caller bin is hashed successfully but launch fails with ENOENT. Empty PATH components (current directory) have the same issue. This also means executable identity and actual executable selection can disagree.

Evidence fixture: /var/folders/q0/5drscxg13d5_5221m5_l3cbc0000gp/T/daf-pr3-relative-path-3buhrv0c. Its state/state.json records adapter codex-cli-v2, a non-null executable hash, and the failed launch. CLI exited 1. The fixture contains a committed baseline and one changed tracked text file; its provider uses native event envelopes.

## Decisions
Resolve bare names using caller PATH to an absolute executable before changing directory, and share resolution with fingerprinting. Keep absolute and explicit relative executable support. Add a subprocess regression without mutating process-global PATH. Snapshot manifest reread and identity inclusion address the previously reproduced deletion race; no additional concrete blocker found in that change.

## Open questions
None beyond the reproduced executable-resolution regression. This review does not claim atomic filesystem snapshots or confidentiality from provider read-only tools.

## Next steps
Coordinator fixes executable resolution, adds the targeted test, runs scoped checks, and merges only the final verified head. Existing cloud/user duplicate files were not modified.

## Implemented follow-up
At coordinator request, updated provider.rs only: shared caller-context resolver canonicalizes explicit paths and PATH candidates before isolated launch; Unix PATH selection skips non-executable regular files. Fingerprinting now uses the same resolver. Added a subprocess regression exercising relative PATH, empty PATH component, explicit relative executable, and a non-executable earlier PATH candidate without changing global test environment.

Validation passed: cargo test --locked -p daf-review --lib provider::tests (6 passed, 0 failed). Formatting applied to provider.rs. Full all-target validation remains coordinator-owned because existing untracked duplicate integration-test names break Cargo discovery in this checkout; no duplicate files changed.

## Post-merge main live finding: Windows executable lookup
Independently triaged the coordinator's live finding against merged provider.rs. Classification: valid P2 portability regression, confirmed by code inspection and Rust's documented Command semantics; not reproduced on Windows. resolve_executable checks PATH directory/name literally, so default `codex` cannot select an existing `codex.exe`. Previously Command performed native resolution. Rust explicitly permits omission of `.exe` on Windows: https://doc.rust-lang.org/std/process/struct.Command.html#platform-specific-behavior . Explicit executable paths without an extension also fail the resolver's canonicalization. Supplying an existing full `.exe` path avoids this specific lookup failure; this is not evidence that the complete workflow works on Windows.

No supported-platform guarantee was located in README or review documentation. Current recorded verification is macOS, and the provider launch regression tests are cfg(unix), so neither the passing macOS suite nor the live main run validates Windows behavior. The defect should remain recorded, rather than dismissed because it does not affect this host. It does not invalidate the observed macOS workflow and resume results.

Recommended follow-up: preserve Windows executable resolution (at minimum omitted `.exe`) with one shared resolver for identity and launch; test real Windows child execution for bare, explicit relative, and absolute paths with and without extension. Validate on an authorized Windows runner locally/server-side, without GitHub Actions. No implementation change or Windows-pass claim made during this triage.
