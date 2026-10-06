# Durable execution handoff

## Done
Implemented `crates/daf-runtime/src/durable.rs`, public module export, runtime SDK/Sled dependencies. API: `DurableLedger::open`, `execute`, `execute_with_dependencies`, `lookup`, `read_effect`. Real SDK `AgentInstance::process_task` invoked for preparation. Protocol and crash acceptance specialists received exact signatures and DTO contract.

## Found
Generic exactly-once external handler effects are impossible here: a process can die after an external mutation and before durable acknowledgement. The implementation explicitly supports pure preparation and atomic declared ledger effects only. Handler purity is a caller contract, not a sandbox-enforced property.

## Decisions
- Before handler invocation, flush durable pending binding of stable task UUID to authenticated principal and canonical complete SDK request plus dependencies. A failed/interrupted handler cannot permit rebinding that ID.
- A result and all immutable principal-scoped declared effect keys commit in one Sled transaction. Colliding effect aborts the entire transaction. Flush before acknowledgement, including cached replays after potentially cancelled flush.
- Result cache replay skips handler invocation. A crash before commit may repeat pure preparation but cannot duplicate a committed ledger effect.
- Require same-principal successful persisted dependencies; reject missing, failed, foreign, duplicate or self dependencies. Framework supplies `inputs.dependency_results[UUID] = predecessor.output`; caller cannot spoof reserved field.
- Successful SDK result.output must serialize `DurableTaskOutput { output, effects: Vec<DurableEffect {key,value}> }`. Returned/persisted result exposes inner output. Failed results persist without effects. SDK task result UUID must match request UUID.
- Bounds: request/result/dependency outputs each 1 MiB; 128 dependencies/effects; effect keys 256 bytes; principal 4096 bytes. Sled owns exclusive process directory lock; one ledger preparation lock serializes local handlers and is cancellation-safe.

## Tests
Passed `cargo check -p daf-runtime`.
Passed two real SDK-backed durable regressions: reopened replay does not reinvoke handler; request/caller conflict; durable dependency output injection; effect collision leaves no result; interrupted SDK timeout retains binding after reopen. Focused command `cargo test -p daf-runtime --lib durable --quiet`: 2 passed, 0 failed.
Strict runtime Clippy passed against final durable implementation and both tests: `cargo clippy -p daf-runtime --all-targets --no-deps -- -D warnings`.
Actual SIGKILL process crash acceptance belongs to remote_e2e specialist, not claimed by these unit regressions.

## Open questions
Single local Sled ledger, no multi-node replication, no effect deletion/compaction policy, no external exactly-once guarantee, no pure-handler sandbox. Precommit aborted tasks retain bindings indefinitely intentionally. Sequential preparation bounds throughput; framework task handlers enforce SDK deadline after lock admission.

## Next steps
Remote protocol: call execute_with_dependencies only with verified transport principal; never client-supplied principal. Crash specialist: kill after execute durable flush before reply, restart same directory, resend same UUID then submit dependent task. Primary: report scope and evidence without conflating repeated pure preparation with duplicate committed effect.
