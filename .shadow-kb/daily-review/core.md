# Daily-use snapshot race finding

## Done
Independently reproduced live review finding against merged PR2 binary, then fixed snapshot consistency in core.rs and added deterministic subprocess regression in workflow.rs.

## Found
Confirmed P2: a tracked unchanged file deleted between initial changed-name collection and patch collection appears in both stable patch reads, but not in the changed-file manifest. Deleted paths have no content hash, so the final snapshot identity previously matched despite different manifests. Actual production CLI run exited 0 with two successful attempts, files=[a], while its prompt and real Git diff included deletion b.
Evidence fixture: /var/folders/q0/5drscxg13d5_5221m5_l3cbc0000gp/T/daf-review-snapshot-race-7e0q5w26 (result.json, prompt, Git repository and wrapper). No real model calls used for reproduction.

## Decisions
Re-read changed names after both patch reads and reject mismatches before provider invocation. Include serialized file/line-bound manifest in snapshot identity as a second defense during final consistency checking and resume. Existing state directories intentionally fingerprint-mismatch after this snapshot identity correction.

## Validation
Passed: cargo test --locked -p daf-review changed_file_manifest_race_is_rejected_before_provider (1 test). Test injects a Git wrapper into only the helper subprocess PATH, deletes committed b.rs after initial listing, asserts explicit collection-change error and zero provider calls. No process-global environment mutation.
Ran cargo fmt --all successfully.

## Open questions
None for this defect; snapshots still use bounded repeated observations rather than a filesystem-wide atomic lock.

## Next steps
Coordinator integrates provider-isolation fix, runs full scoped checks, and reviews a fresh workflow run with a new state directory before publishing follow-up.
