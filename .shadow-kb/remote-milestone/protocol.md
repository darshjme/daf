# Remote worker protocol milestone

## Done
Implemented `crates/daf-runtime/src/remote.rs` and module export; runtime bytes/tokio-util/ring dependencies added (execution owns SDK/sled).
- RemoteWorker binds real TCP, optionally wraps with the existing TLS transport, uses actual bounded DdalCodec encode/decode. One request/reply per connection, stream 1 Data frames only, flags zero.
- Nonloopback listener and client addresses require TLS. Loopback plaintext is for development only. TLS client always verifies server via configured CA; mTLS optional configuration independently of credential authentication.
- CredentialVerifier resolves canonical principal and explicitly authorizes SDK task. Authorization defaults deny. StaticCredential stores only domain-separated HMAC-SHA256 digest, uses ring constant-time verification, requires a 32+ byte token and explicit task-name allowlist. No Debug for credentials or wire requests.
- TaskEnvelope carries SDK task (task ID doubles as idempotency ID) plus predecessor UUIDs. DurableTaskExecutor delegates to DurableLedger::execute_with_dependencies with the authenticated principal.
- max_sessions bounds sessions and execution futures; max_frame_bytes bounds both request and reply, session_timeout bounds handshake/read/execute/observer/write. Admission waits when full. Shutdown stops admission, aborts and drains all owned JoinSet tasks; no detached futures.
- ReplyObserver exposes an optional async after-execution/pre-reply observability seam. Crash acceptance helper owns the deterministic gate; no production crash switches.

## Evidence and tests
`cargo check -p daf-runtime` passed.
`cargo test -p daf-runtime remote::tests --lib` passed 7/7: TCP auth+task scope+payload roundtrip; TLS/mTLS roundtrip using disposable public fixtures; fail-closed configuration/credential provisioning; oversize request never executes; malformed DDAL control frame never executes; bounded sessions cancellation drops execution futures; session deadline drops execution.
`cargo clippy -p daf-runtime --all-targets -- -D warnings` passed before adding the final two regression tests. Primary final workspace strict Clippy will cover final combined tree.

## Decisions
Credential authority comes solely from verifier, never request-supplied principal. Explicit task-name scope belongs to verifier. JSON envelopes inside DDAL are this milestone's application protocol; existing DDAL bincode handshake is separate and not implied here. TLS identity does not automatically authorize task execution.

## Open questions and limits
- Durable exactly-once guarantee applies only to ledger effects plus receipts, not network/external side effects. Custom RemoteTaskExecutor implementations must honor their own durability contract.
- Timeout/shutdown can leave pending receipts. Caller retries must reuse identical task ID and original content; ledger policy decides pending recovery.
- Tokens require entropy and secure provisioning; verifier rotation currently requires replacement/restart. No credential expiry/revocation service, TLS certificate-to-principal mapping, fleet discovery, cross-node consensus, streaming tasks or multiplexing.
- Error replies from custom executors may disclose their own messages; executor implementations must keep secrets out of errors.
- max_sessions bounds application-owned accepted sessions; OS listener backlog remains kernel-managed. No application queued task buffer.
- Durably committed output too large for reply is retained in ledger, but client gets protocol failure; deployments must bound handler result sizes or choose adequate frame limit.

## Next steps
Remote acceptance agent verifies process SIGKILL after commit/before reply and restart/replay/dependency outputs with DurableTaskExecutor. Primary runs combined workspace tests, strict Clippy, Rustdoc and documents actual guarantee scope.
