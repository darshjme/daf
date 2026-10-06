# 17 — Dependency cancellation semantics

## Done
Added real SDK regression: caller timeout drops ledger preparation; pending predecessor produces no completed result; failed predecessor persists a failure; neither admits dependent handler. Independent work still succeeds after cancellation, proving ledger lease release.

## Found
Pending binding retains stable request identity after interruption, while successful dependency lookup requires a completed same-principal result. Existing OnOutput/mission routes are separate contracts.

## Decisions
Keep cancellation pending, not falsely completed. Pure preparation may rerun for the same request after interruption.

## Actual validation
`cargo test --locked -p daf-runtime --quiet`: 86 unit tests + 2 doctests passed, zero failed/ignored. `cargo clippy --locked -p daf-runtime --all-targets --no-deps -- -D warnings`: passed. Final added deadline representability gate: all11focused remote tests passed, followed by final locked strict Clippy and runtime diff check. Primary combines workspace validation. No GitHub Actions, commits, external services or lingering task processes.

## Open questions
No distributed cancellation propagation or external-effect rollback. SDK handler purity remains an explicit contract.

## Next steps
Primary distinguish repeated pure preparation from duplicated committed ledger effects; no new DAG scheduler claims.
