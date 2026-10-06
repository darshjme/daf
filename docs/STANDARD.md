# DAF advanced agent systems standard — draft 0.1

By **Darshankumar Joshi**. This is a project engineering specification, not an external certification or a claim that DAF is AGI or ASI. An agent framework coordinates execution; intelligence depends on the models, tools, environments and evaluations connected to it.

The standard is deliberately demanding. A requirement is satisfied only by executable evidence for the complete advertised path. A type, configuration field, mock worker, passing codec benchmark or successful dispatch does not establish end-to-end capability.

## Conformance levels

| Level | Required evidence |
| :--- | :--- |
| L0: components | Public APIs compile and have meaningful behavioral tests; limitations are published. |
| L1: local execution | Actual handlers/commands run; dependency failures, deadlines, cancellation and reports reflect observed execution. |
| L2: integrated agents | Authenticated remote worker → tool/model execution → durable result → dependent task; restart, disconnection and denial tests pass. |
| L3: resilient deployment | Resource isolation, crash recovery, idempotency, migration, backup/restore, bounded load and adversarial acceptance tests pass on documented infrastructure. |
| L4: evaluated autonomous systems | Versioned model/task/environment evaluations establish task quality, cost, reliability, oversight effectiveness and reproducibility. |

Conformance is per path, not per repository. Current DAF provides component libraries and a local command path; it does not claim L2–L4. A model provider integration and a serious evaluation harness remain required work. There is no “ASI-certified” level.

## Requirements and acceptance gates

**MUST** is a release gate for a path that claims the capability. **SHOULD** requires a documented rationale if omitted. A missing gate blocks the relevant conformance claim, not honest distribution of an experimental library.

| ID | Requirement | Acceptance evidence |
| :--- | :--- | :--- |
| EX-01 | MUST execute registered workers and propagate their actual result. | Worker produces an independently checked artifact; dispatch without a worker fails. |
| EX-02 | MUST preserve dependency and conditional-edge semantics. | Out-of-order DAG, cycle, success, failure, unconditional cleanup and parallel-edge tests. Unsupported output routing fails explicitly. |
| EX-03 | MUST bound concurrency, queues, payload sizes and deadlines. | Saturated queues, oversized frames, timeout and concurrent limit tests. Configuration limits must affect behavior. |
| EX-04 | MUST terminate owned work on cancellation and shutdown. | Listener rebinding, channel EOF, task abort and process termination tests; child descendants documented or isolated. |
| EX-05 | MUST prevent retries from silently duplicating external effects. | Idempotency key and restart/retry tests against a real effect store. Not implemented by dispatch bookkeeping alone. |
| SE-01 | MUST authenticate a remote principal before privileged work. | Missing/wrong credentials and untrusted certificates rejected; identity cannot be forged through an agent-name field. |
| SE-02 | MUST apply configured authorization middleware and least privilege. | Denied request never reaches handler or tool; grant scope is explicit. |
| SE-03 | MUST protect secrets in persistence and diagnostics. | AEAD tamper rejection, wrong-password rejection, rotation/reopen and Debug redaction tests. |
| SE-04 | MUST isolate untrusted tools and constrain filesystem/network effects. | Escape and resource exhaustion tests within an explicit sandbox; argv execution alone is not isolation. |
| ME-01 | MUST preserve authoritative evidence and ownership. | Source, scope, timestamps and revision retained across write/restart/merge; retrieval is evidence, not an instruction override. |
| ME-02 | MUST avoid data loss during replacement and consolidation. | Failed write leaves original usable; repeated consolidation does not destroy unreplaced records. |
| ME-03 | MUST state retrieval semantics accurately. | Ranked/filtered limit tests; embedding or semantic claims require a real index and quality evaluation. |
| DI-01 | MUST define delivery, ordering, timeout and reconnect semantics. | Connection-loss and duplicate/out-of-order scenarios; “exactly once” requires durable proof. |
| DI-02 | MUST report only observed health and availability. | Uninitialized/broken subsystem is unhealthy or unknown; listener alone cannot establish worker readiness. |
| DI-03 | MUST retain durable mission state for restartable workflows. | Kill between effect and acknowledgement, reopen and resume; no invented completion. |
| OB-01 | MUST distinguish success, failure, skipped, timeout and cancellation. | Machine-readable ordered reports match actual outcomes and nonzero failure exit codes. |
| OB-02 | SHOULD expose correlation IDs, sanitized diagnostics and operational metrics. | Trace task/mission/request across boundaries; documented metric definitions and no secret leakage. |
| ML-01 | MUST expose explicit provider/tool contracts before advertising model orchestration. | Deterministic provider fixtures plus documented live-provider acceptance; errors, streaming and usage accounted for. |
| ML-02 | MUST evaluate intelligence separately from infrastructure throughput. | Versioned datasets, model settings, evaluator, quality baseline, cost and repeated trials. Bytes saved are not intelligence gained. |
| ML-03 | MUST support enforceable oversight for consequential autonomous effects. | Review/deny/cancel can prevent execution; decisions are persisted and attributable. |
| DX-01 | MUST ship runnable examples and preserve existing project files. | Fresh init → sample mission → result; repeat init fails without overwriting. |
| DX-02 | MUST publish reproducible local validation and precise limitations. | Locked build/test/lint/doc/acceptance commands with actual results and environment. No dependency on GitHub Actions. |

## Release evidence

A conformance record must name the source commit, toolchain, environment, commands, outcomes, fixtures, unsupported paths and residual risks. Tests of local stand-ins must be identified as simulations and cannot stand in for tests of DAF public APIs. Benchmark reports must include hardware, payload/workload, sample size and latency distribution; targets are not measurements.

The audit report in [AUDIT-2026-10-06.md](AUDIT-2026-10-06.md) records the repairs and current gaps. Conformance must be rechecked after changes to security, execution, storage or configuration.

The [remote milestone](REMOTE.md) adds real process-crash evidence for scoped ledger effects and receipt replay (EX-05, DI-01, DI-03), authenticated task scope (SE-01/02), bounded sessions and cancellation (EX-03/04). These are path-specific results: they do not establish external-effect idempotency, tool sandboxing, fleet consensus or model intelligence.
