# Authenticated durable remote execution

DAF now provides a reusable remote worker and client in `daf_runtime::remote`, backed by `daf_runtime::durable`. This is a single-node execution path with authenticated task scope, durable receipts and explicit dependencies. It does not provide a fleet scheduler, consensus, a tool sandbox or an intelligence model.

## Run the example

Build both binaries, generate a development credential, then start the worker:

```sh
cargo build --locked -p daf-example-remote-worker --bins
export DAF_REMOTE_TOKEN="$(openssl rand -hex 32)"
target/debug/daf-example-remote-worker --db /tmp/daf-remote-demo --listen 127.0.0.1:7474
```

In a second terminal, set the same `DAF_REMOTE_TOKEN` securely and run:

```sh
target/debug/daf-example-remote-client --mission /tmp/daf-remote-mission.json
```

The parent sums 20 + 22. Its dependent adds 8 to the persisted parent output, yielding 50. The client writes and syncs its task manifest before submitting requests. Repeating the command uses the original IDs and returns cached results. Stop the worker with Ctrl-C, restart with the same database, and repeat the client command to replay both results. Keep the manifest: generating new task IDs creates new work.

These binaries deliberately accept loopback addresses only. The library requires TLS for nonloopback addresses. Configure trusted certificates through `TlsConfig`; mutual TLS is optional. Provision high-entropy tokens separately and never place credentials in task inputs or logs. `StaticCredential` grants explicit task names to a server-derived principal; custom verifiers default to denying authorization. Certificate verification and task authorization are separate checks.

## Commit and replay contract

```mermaid
sequenceDiagram
    participant C as Client
    participant W as Authenticated worker
    participant H as SDK preparation handler
    participant L as Durable ledger
    C->>W: Stable task ID, inputs, dependencies, credential
    W->>W: Verify principal and authorize task
    W->>L: Bind principal and canonical request; flush
    L->>H: Execute with persisted dependency outputs
    H->>L: Prepared output and declared effects
    L->>L: Atomically commit effects and result; flush
    W--xC: Reply lost or process killed
    C->>W: Retry identical request after restart
    W->>L: Check binding and flush stored receipt
    L-->>C: Replay committed result without handler invocation
```

An SDK handler returns `DurableTaskOutput` encoded in `SdkTaskResult.output`. Its declared `DurableEffect` entries are immutable, principal-scoped JSON values. The ledger atomically inserts all effects and the result in one Sled transaction, then flushes before acknowledgement. A conflicting existing effect aborts the entire transaction. A task ID binds to its principal, complete original task specification and ordered dependency list; altered requests are rejected even after interrupted preparation. Failed SDK outcomes are stored and replayed as failures.

Handlers must prepare values without performing external effects. Preparation may run again after a crash before commit. Rust handlers are trusted code; this contract does not sandbox them. Emails, payments, shell commands and external databases require their own idempotency or transactional integration. The guarantee is atomic ledger effects plus receipt replay, not exactly-once arbitrary execution.

Dependencies must already have successful durable results for the same principal. Their outputs are injected under reserved `inputs.dependency_results`, keyed by UUID. Missing, failed, duplicate or self dependencies fail before handler invocation. A client submits the dependent after its predecessor completes; the server does not schedule a graph automatically.

## Limits and operation

Each connection carries one JSON request and response in DDAL Data stream 1, with zero flags. This application protocol does not use the separate DDAL bincode handshake helpers. Default worker admission is 16 sessions; one configured deadline covers TLS, framing, execution, observation and response. Shutdown aborts and drains owned sessions. The ledger serializes preparation and Sled exclusively locks its directory, so this implementation favors correctness over parallel write throughput.

The ledger caps original requests and prepared results at 1 MiB, dependency output aggregation at 1 MiB, dependencies and effects at 128 each, effect keys at 256 bytes and principal identifiers at 4096 bytes. Remote frame limits include the 21-byte DDAL header. Configure enough reply capacity for the result envelope: a committed result too large for the configured frame remains durable but cannot be delivered with that limit. Session expiration can leave a bound, uncommitted request; retry it unchanged. Storage records have no retention or compaction policy yet.

No automatic reconnect/retry loop, credential expiry service, cross-node replication, untrusted-tool isolation or load benchmark is included. Custom executor error messages must omit secrets. Test-only environment gates in the example enable deterministic crash acceptance and must remain unset for normal use.

## Reproduce acceptance

```sh
cargo test --locked -p daf-runtime durable::tests --lib
cargo test --locked -p daf-runtime remote::tests --lib
cargo test --locked -p daf-example-remote-worker --test recovery
```

The process test uses real TCP/DDAL, credential verification, an SDK handler and Sled. It kills a separate worker with OS SIGKILL after durable commit and before reply, reopens its database, restarts it, retries the stable request and verifies one handler invocation and one immutable effect. A dependent produces 50 from recovered output 42; its own committed replay is checked after a second process kill. Invalid credentials and altered request identities are rejected. This establishes the tested process-crash boundary on the local filesystem, not power-loss durability on every storage device.

Final combined validation is recorded in [REMOTE-MILESTONE-2026-10-06.md](REMOTE-MILESTONE-2026-10-06.md).
