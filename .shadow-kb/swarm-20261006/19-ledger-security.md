# 19 — Durable ledger security

## Done
Fixed combined resolved-input admission: caller request under1MiB plus predecessor outputs under1MiB could otherwise produce nearly2MiB task input. Added final resolved-task1MiB gate before SDK invocation. Real SDK regression creates two600kB predecessors, rejects oversized dependency aggregate and combined caller+one predecessor expansion; caller/dependency gates add zero handler calls.

## Found
Existing dependencies count gate runs before reads. Tests also reject foreign principal, duplicate/self dependency and129dependencies. Records/effects remain atomic Sled transaction followed by flush; no repair broadens external effect guarantees.

## Decisions
Preserve1MiB request/result/dependency and now final resolved task limits. Retain immutable principal scoped effects and durable UUID binding.

## Actual validation
`cargo test --locked -p daf-runtime --quiet`: 86 unit tests + 2 doctests passed, zero failed/ignored. `cargo clippy --locked -p daf-runtime --all-targets --no-deps -- -D warnings`: passed. Final added deadline representability gate: all11focused remote tests passed, followed by final locked strict Clippy and runtime diff check. Primary combines workspace validation. No GitHub Actions, commits, external services or lingering task processes.

## Open questions
Sled directory is local and exclusively locked; no database encryption, replication, quota/retention policy or hostile handler sandbox. Allocation of handler-owned values cannot be prevented by post-preparation serialization limits.

## Next steps
Primary keep original SIGKILL result/effect evidence and report this wave as boundary strengthening, not another crash claim.
