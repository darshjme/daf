# Execution specialist handoff

## Done
Audited graph algorithms, scheduling, cancellation, orchestration, SDK API, AI task boundary and evaluation coverage (roles 1–7). Modified daf-graph, daf-orchestrator, daf-sdk, and added tests/integration/tests/execution_acceptance.rs. No commits or GitHub Actions.

Repairs:
- Actual-state wave planning now discovers OnFailure recovery branches that success-only simulation previously omitted.
- Explicit Always cleanup edges run after failed/skipped predecessors. Kind-default DependsOn/DataFlow require success. This is a documented semantic correction: missing conditions on those two edge kinds now resolve to OnSuccess; explicit Always retains terminal-completion semantics.
- Every incoming parallel edge must satisfy its condition. Terminal impossible branches are skipped transitively, without skipping valid Always cleanup.
- OnOutput predicates fail closed with UnsupportedOutputCondition before handler invocation: handler API returns only Result<(), String>, so there is no output predicate evaluator.
- Reject zero concurrency, duplicate node IDs in serialized graphs, concurrent execution of the same GraphExecutor and concurrent duplicate mission IDs. Repeated in-memory add_node is idempotent rather than creating an unreachable duplicate.
- Graph caller cancellation/drop cancels outstanding nodes; cancellation interrupts hanging lifecycle callbacks. Spawned work is held in JoinSet. Progress now records timing/wave state and freezes elapsed duration after completion.
- SDK builder middleware now runs on process_message. Before this fix a configured AuthMiddleware was bypassed entirely.
- SDK task.timeout and max_concurrent_tasks now enforced; RAII permits release on timeout/caller cancellation.
- Orchestrator max_concurrent_missions now enforced with an admission set and cancellation-safe mission lease.
- Specialist routing now requires every requested capability. Duplicate completions cannot underflow u64 load counters.
- Baseline own-crate unused import and small Clippy issues repaired.

## Found
Severity high: authentication middleware bypass, recovery branch omission, false output-predicate evaluation, unenforced admission limits, incomplete capability matching.
Severity medium: Always cleanup skipped on failure, parallel edges inconsistently evaluated, cancellation returned Ok on the final wave, live progress fabricated zeros, duplicate graph node ID corruption, router load underflow.

Actual public-API acceptance connects GraphExecutor failure recovery -> Orchestrator registered WorkerHandler -> SDK task handler. It verifies SDK auth denial/allowance and router/agent load release. Existing old integration test suites largely model local stand-in structures; passing those cannot prove actual runtime behavior.

## Decisions
No unsupported AGI/ASI claims. Framework execution reliability is measurable; intelligence capability requires a real model/tool adapter and held-out evaluation evidence. Current worker and graph boundaries are in-process execution infrastructure. Output routing remains explicitly unsupported rather than invented. Mission phases/tasks remain sequential as source docs state; this patch does not introduce distributed execution.

## Tests
Passed first focused run: graph 69, orchestrator 78, SDK 87 unit tests; 2 doctests passed, 10 SDK doctests ignored.
Passed subsequent focused run: graph 71, orchestrator 80, SDK 87 unit tests; same doctest outcomes. This includes failure routing, cleanup, unsupported output, zero concurrency, duplicate IDs, parallel edges, authentication, task deadlines, cancellation callbacks, caller drop and mission admission regressions.
Passed real exported-type integration acceptance (1 test) twice.
Final focused run passed: graph 71, orchestrator 82, SDK 88 unit tests; 2 doctests passed, 10 SDK doctests ignored. Real execution_acceptance passed again against final code. SDK task admission, router capability/load and frozen progress changes are included. A transient doctest compile failure while converting Environment Default was corrected and the complete final rerun passed.
Strict Clippy passed: cargo clippy -p daf-graph -p daf-sdk -p daf-orchestrator --all-targets --no-deps -- -D warnings. Initial upstream/default and own baseline lint issues were repaired; final exit was zero.

## Open questions / limitations
- SDK lifecycle start/stop remains an in-process lifecycle wrapper; transport registration/heartbeat/draining comments describe future runtime integration. stop does not drain arbitrary message/event handlers.
- Retry semantics differ across APIs: graph retries all handler errors; orchestrator uses mission RetryPolicy and documents that task.max_retries is ignored. Idempotency remains handler responsibility.
- Graph TaskHandler has no structured output, output-schema validation, model adapter or intelligence evaluation contract.
- Router uses heuristic capability/load/affinity scoring, not learned expert selection. Required capabilities now constrain eligibility but role inference is still string-based.
- Graph validation does not provide a secure sandbox or durable transactional replay. Third-party handlers can perform external side effects before cancellation.
- Benchmarks, held-out agent quality, adversarial tool-use evaluation, human correction, cost/latency curves, model-specific adapters and distributed fault tests are still needed to justify advanced-agent capability.

## Next steps
Parent: include corrected edge defaults and unsupported output routing in docs; run workspace validation after all specialists merge; describe measured advanced-agent standard honestly. Do not count ignored SDK docs as passed. Preserve local/server testing policy, no Actions.
