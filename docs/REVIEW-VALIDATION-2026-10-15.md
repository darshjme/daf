# Changed-file review verification — 2026-10-15

Implemented on main baseline `dafe20d95bb60804c2ac2d4e00f6e833d28d63df`. Verification ran locally on macOS; no GitHub Actions were used.

| Check actually run | Result |
| --- | --- |
| `cargo test -p daf-orchestrator --lib --offline` | 86 passed |
| `cargo test --locked -p daf-review -p daf --all-targets` | 30 passed: 8 CLI unit, 3 CLI integration, 7 review unit, 12 workflow integration (one subprocess helper) |
| `cargo clippy --locked -p daf-review -p daf --all-targets -- -D warnings` | Passed |
| `cargo fmt --all --check` | Passed |
| `python3 scripts/check_local_cli.py` | All 17 checks passed |
| `git diff --check` | Passed |

The full workspace suite was not rerun. The changed packages and existing orchestrator path were tested directly.

## Recovery and failure evidence

Tests execute real Git repositories, registered DAF workers and subprocess JSONL providers. They cover:

- Identical resume without additional provider invocations; different bytes or model configuration reject reuse.
- Raw changed bytes invalidate cache even when Git normalizes them into an identical patch.
- Actual process kill after a completed reviewer and during the next attempt. Restart preserves the completed result, marks the unfinished attempt interrupted, and refuses another call without explicit authorization.
- A concurrent writer is rejected without changing journal bytes or invoking a provider.
- Invalid paths and malformed findings trigger bounded retries while retaining reported usage for every attempt.
- Nonzero exits exhaust cumulative retry budgets across resume. Missing counters remain unknown.
- Output overflow and timeout are uncertain outcomes; terminal usage survives a later provider hang.
- Binary changes fail before provider dispatch. Corrupt cached results fail closed.

An initial parallel test run exposed a lock inherited briefly by unrelated forked children. Explicit unlock in the RAII guard fixed it; the final default-parallel suite passed. An initial lint failure from schema placement after the test module was fixed before the passing lint run.

## Live model acceptance

The full `daf review` CLI ran against a temporary Git fixture using authenticated `codex-cli 0.162.0-alpha.17.2`, model `gpt-6.1-sol`, timeout 120 seconds, zero retries. The working change removed `/ 100` from a percentage calculation. The correctness specialist reported the concrete regression: `discounted_total(100, 20)` becomes `-1900` instead of `80`. The security specialist returned no findings.

| Attempt | Input tokens | Cached input tokens | Output tokens |
| --- | ---: | ---: | ---: |
| Correctness | 24,486 | 0 | 111 |
| Security | 24,486 | 0 | 37 |
| Known totals | 48,972 | 0 | 148 |

Counts came from native `turn.completed.usage` events. Repeating the identical CLI command returned the same decoded report with two total attempts and unchanged usage. It ran without provider network access, demonstrating reuse of the committed results.

A preceding sandboxed provider attempt exited without a terminal usage receipt. Its separate journal retained an interrupted attempt with null usage and blocked automatic retry. Its billing is unknown and excluded from the known totals above; the successful network-enabled run used a separate run directory.

The live smoke verifies one seeded regression and native usage reporting, not review quality across real repositories. Finding validation checks schema and locations, not semantic correctness. Model aliases can change remotely. External model calls cannot be made exactly once by a local journal. Unix process-group cleanup handles timeout/cancellation; non-Unix cleanup covers the direct child. A hard kill of the workflow can prevent cleanup, so restart records uncertainty.

## PR #2 review corrections

Independent pre-merge review reproduced two defects that the initial suite did not cover. Snapshot collection now uses `symlink_metadata` before classifying a missing file, so dangling and resolved symlinks both fail before provider invocation. Multiple terminal completion events now invalidate usage counters instead of presenting the last receipt as the entire attempt; two- and three-receipt regressions cover this ambiguous stream. The final changed-package suite contains 30 passing tests. Scoped clippy and formatting checks passed after both fixes.

The first post-review parallel run exposed a timing-sensitive hang fixture: its one-second deadline could expire before Python emitted its terminal receipt. The receipt-preservation test now permits five seconds for startup and hangs for thirty seconds, retaining the timeout assertion.
