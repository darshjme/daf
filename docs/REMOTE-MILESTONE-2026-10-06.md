# Remote execution milestone — 2026-10-06

Implemented by Darshankumar Joshi as a continuation of the DAF audit on `audit/e2e-20261006`, reviewed in https://github.com/darshjme/daf/pull/1.

## Outcome

Reusable authenticated TCP/TLS remote worker/client now invokes real SDK handlers through a durable Sled ledger. Stable task identity binds principal and canonical request. Declared immutable storage effects and results commit atomically, flush before acknowledgement, and replay after worker restart. Same-principal dependent tasks consume persisted predecessor outputs. The example client persists its original task manifest before transmission.

The guarantee covers declared ledger effects and result receipts. Preparation may repeat before commit and must be free of external effects. Payments, emails, shell commands and external database mutations require separate idempotency. No tool sandbox, fleet consensus or AGI/ASI capability is established.

## Verified locally

Rust/Cargo 1.94.1, macOS arm64; locked dependencies and no GitHub Actions.

- `cargo test --locked --workspace`: **1005 passed, 0 failed, 15 ignored**, including doctests. Ignored cases remain unverified.
- Actual subprocess SIGKILL acceptance: commit-before-reply → kill → database reopen → restart → cached replay without another handler invocation. Recovered42 feeds dependent50; both immutable effects survive a second SIGKILL.
- Seven remote protocol regressions include real TCP authorization, TLS/mTLS, malformed/oversized frames, bounded sessions, deadlines and shutdown.
- Two SDK-backed ledger regressions cover replay, principal/request conflicts, effect collision atomicity, dependency outputs and interrupted preparation binding.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`, warning-free Rustdoc, formatting and diff checks passed.
- CLI binary build and all 13 local acceptance checks passed.
- Both example binaries built. Real client/worker demo produced42 and50; repeated submission and graceful worker restart returned identical result JSON. All started workers were reaped.

Combined testing exposed two timing defects in test infrastructure. The old graph simulation cancellation test now uses paused Tokio time rather than racing wall-clock completion. The crash helper now publishes readiness/commit markers through atomic rename so readers cannot observe empty files. The final full suite above passed after both fixes.

## Scope and next engineering targets

The ledger serializes preparation and locks one local Sled directory. There is no multi-node replication or automatic graph scheduler, reconnect loop, credential expiry service, retention policy, sandbox or measured performance claim. A handler result may commit but exceed a deployment's reply frame limit; operators must budget envelope overhead. Process-kill evidence does not prove power-loss guarantees on every filesystem/device.

See [REMOTE.md](REMOTE.md) for runnable commands and protocol/commit semantics. Future work should add sandboxed tools, explicit model-provider contracts with quality evaluation, operational diagnostics, retention and independently measured throughput before expanding capability claims.
