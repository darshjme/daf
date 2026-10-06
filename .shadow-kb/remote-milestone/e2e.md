# Remote OS crash acceptance and development worker

## Done
Added workspace package `daf-example-remote-worker` with runnable loopback development worker and an actual subprocess integration test under `examples/remote-worker/tests/recovery.rs`. Worker uses the exported runtime RemoteWorker, StaticCredential and DurableTaskExecutor, SDK AgentBuilder/TaskHandler, and DurableLedger. The prepare-only sum handler returns transactional ledger effects; it performs no external business action. It consumes framework-injected durable dependency outputs.

The acceptance test launches the compiled real worker executable (Cargo binary env resolution; clean workspace test automatically builds it), rejects empty/wrong credentials before any handler invocation, submits a stable-ID parent task, waits for an observer marker strictly after DurableTaskExecutor returned and before reply serialization, sends actual OS SIGKILL, verifies signal 9 and reaps the process, confirms the in-flight client failed, opens the ledger after process death and verifies committed result/effect, restarts on the same DB, retries the identical request and proves no second handler invocation, rejects changed payload under the same task ID, submits a dependent task consuming recovered parent output, retries it from cache, then SIGKILLs/reopens again to verify both effects/results. RAII child guards kill/reap on failure or timeout; no detached child remains.

## Found
Verified real postcommit/pre-reply recovery through the integrated APIs. Parent values 19+23 persist sum42 with effect applied1; dependent value8 plus durable predecessor42 persists sum50 with effect applied1. This is exactly-once ledger effect/result commit for prepare-only handlers, not exactly-once arbitrary external actions. Test-only invocation log proves replay avoids handler rerun after committed crash. Loopback plaintext worker is a development fixture; TLS integration belongs to protocol specialist validation.

## Decisions
Test resides in the example package so `CARGO_BIN_EXE_daf-example-remote-worker` is resolved by Cargo automatically, avoiding tests that depend on a previously hand-built binary. Public worker args `--db PATH`, `--listen LOOPBACK` (default127.0.0.1:7474), `--help`. Authentication uses `DAF_REMOTE_TOKEN` with minimum32bytes and exact permitted task `sum`; canonical principal development-operator. Worker intentionally rejects nonloopback addresses. Test-only environment variables `DAF_TEST_ADDRESS_FILE`, `DAF_TEST_HANDLER_LOG`, `DAF_TEST_HOLD_AFTER_COMMIT_FILE` provide readiness, diagnostic invocation evidence and observer gate. The hold variable must never be enabled for normal demo, as it blocks successful replies until session timeout or crash. Primary owns client example and docs.

## Validation
Passed `cargo test -p daf-example-remote-worker --test recovery`: 1 passed, 0 failed, actual subprocess SIGKILL recovery completed1.18s. Test has20s overall and5s readiness/commit/client bounds. Initial compile had outdated SDK durable import; corrected to runtime durable and removed stale unused TcpStream import before passing. Passed `cargo clippy -p daf-example-remote-worker --all-targets -- -D warnings` (1.28s). No Actions, commits or production deployment. No OS power failure/storage hardware fault was simulated; test covers OS process death after sled flush.

## Open questions
Prepare-only guarantees do not extend to payments, cloud mutations or other handler external side effects. Client mission files must persist exact task IDs/requests to use replay safely. Credential principal and task authorization must remain canonical across restart.

## Next steps
Primary integrate client persisted-mission demonstration, workspace checks, documentation and review. Protocol specialist owns real TLS/mTLS acceptance and remote admission behavior. Execution specialist owns durable pending/conflict/collision regressions and semantics. No changes to runtime code by this specialist.

## Primary integration verification
Final workspace run exposed an empty-marker file publication race; readiness and commit markers now publish via atomic rename. Final workspace run passed 1,005 tests with 0 failures and 15 ignored; strict final Clippy and all 13CLI checks passed. Persisted-manifest client demo produced42/50 and identical replay across graceful worker restart.
