# PR2 core snapshot and resume review

## Done
Reviewed core.rs durable journal, snapshot identity, retry accounting, cache validation and concurrent writer behavior at initial head 73157e6. Reproduced and fixed one input-validation defect in core.rs; added regression in tests/workflow.rs.

## Found
P2: Path::exists follows symlinks, so a tracked regular file replaced with a dangling symlink bypassed regular-file enforcement and was treated like deletion. Existing symlinks were rejected, making input admission depend on whether the target happened to exist. A real temporary Git repository and executable fixture provider reproduced successful exit with two provider attempts before the fix. Evidence directory: /var/folders/q0/5drscxg13d5_5221m5_l3cbc0000gp/T/daf-review-symlink-zwuibcd_. No other actionable core/resume defect identified in this pass.

## Decisions
Inspect symlink_metadata directly. Only NotFound means deletion; other filesystem errors propagate, and every existing nonregular entry (including dangling symlinks) is rejected. No implementation changes beyond this scoped admission fix.

## Validation
Passed: cargo test --locked -p daf-review changed_symlinks_are_rejected_before_provider_even_when_dangling (one test exercising dangling and resolved targets; asserts zero provider calls).
Passed: cargo fmt --all --check.
Full suite left to coordinator's final integrated verification.

## Open questions
None for the fix. This review is not a guarantee of semantic correctness of model findings.

## Next steps
Coordinator review diff, combine provider specialist findings, run final integrated checks, update PR and merge exact reviewed head before daily-use verification.
