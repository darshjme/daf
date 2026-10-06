# 10 — Worker timeout and shutdown

## Done
Reviewed remote session timeout includes TLS/request/execution/observer/reply. Added silent stalled peer regression proving max_sessions1 slot becomes usable after expiry; added shutdown regression proving a hanging postexecution observer future drops and worker shutdown terminates. Reject unrepresentable Duration::MAX configuration before Tokio deadline construction.

## Found
Existing JoinSet owns session tasks; timeout drops work and shutdown aborts/drains tasks. Server deadline closes the connection; caller may observe Protocol(peer closed), rather than an explicit Timeout response.

## Decisions
Preserved existing bounded cancellation semantics; no detached future or new background architecture.

## Actual validation
`cargo test --locked -p daf-runtime --quiet`: 86 unit tests + 2 doctests passed, zero failed/ignored. `cargo clippy --locked -p daf-runtime --all-targets --no-deps -- -D warnings`: passed. Final added deadline representability gate: all11focused remote tests passed, followed by final locked strict Clippy and runtime diff check. Primary combines workspace validation. No GitHub Actions, commits, external services or lingering task processes.

## Open questions
Timeout cannot roll back an external side effect. Observers must not perform unsupported irreversible effects; shutdown after ledger commit can leave no reply while durable replay remains available.

## Next steps
Primary preserve connection closure wording and existing SIGKILL replay evidence. This wave did not repeat a process crash campaign.
