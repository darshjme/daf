# Protocol, transport, registry and runtime specialist handoff

## Done

Reviewed binary framing, handshake, logical channels, TCP/TLS/in-process/Unix transport, routing, registry discovery, runtime supervisor/signal paths and core resource contracts. Repaired concrete implementation defects in owned crates. No commits or Actions were created.

- DDAL codec validates actual payload size, declared length and checksum on encode; custom codec limits cannot bypass the protocol's 16 MiB ceiling. Incompatible wire versions reject before payload buffering.
- ChannelPool retains the outbound receiver and exposes `take_outbound_receiver`; sends previously always failed because the receiver was dropped during open. Added bounded queue/backpressure regression and channel capacity reuse after close. Lifecycle changes serialize allocation/close and avoid channel ID wraparound.
- Removed the unusable handshake oneshot API: `perform_handshake_server` now returns `HandshakeRequest`; caller validates it and explicitly sends a decision via `complete_handshake_server`. Updated actual wire-format documentation. Debug redacts bearer tokens.
- Route removal and stale eviction now mutate/delete atomically under DashMap shard locks rather than deleting a route inserted between two lock acquisitions.
- TLS `verify_peer` previously did nothing: server always used no_client_auth and clients never presented certificates. It now uses WebPkiClientVerifier with the configured CA, presents client credentials, rejects missing CA and zero timeout, and bounds TCP connect. Added real local TCP TLS tests proving authenticated data transfer and anonymous-client rejection.
- Added clearly labeled public disposable TLS fixtures (CA certificate, test certificate, PUBLIC TEST KEY; no CA private key retained). Not deployment credentials; leaf expiry is October 2036.
- In-process close drops its sender immediately so peer EOF does not require Drop. Closed writes fail, empty writes cannot manufacture EOF, zero-length reads do not consume data. Bus matching uses atomic entry access and collision-free length-prefixed endpoint keys.
- Unix binding no longer unlinks arbitrary existing regular files or active socket paths. Explicit cleanup accepts only actual socket files. Listener Drop checks socket type and recorded device/inode identity before unlinking. Added regular-file preservation regression.
- Registry duplicate registration is atomic, with a 16-thread one-winner regression. Health teardown occurs while the registration shard remains locked, avoiding deleting a newly registered agent's health record.
- Runtime programmatic shutdown releases its OS-signal listener; first SIGINT now proceeds to teardown. Notify wait registration closes a lost-notification race. Shutdown callers serialize. Start rejects duplicate startup; owned background tasks abort on cancellation/return, including deadline watcher. Shutdown before start is allowed.
- Runtime periodic health reports unconnected subsystem probes as Unknown; empty health sets are Unknown. Unimplemented config reload no longer increments a successful-reload metric.

## Found / limitations

Critical repaired defects were executable behavior, not formatting issues. Their previous tests largely inspected constructors/accessors and missed outbound channel data flow, mTLS policy, and full runtime shutdown.

Remaining architectural constraints must be visible in release documentation:

1. Runtime bootstrap remains a coordination shell: transport/memory/registry/logger/orchestrator initialization functions mostly log, without creating and retaining functioning subsystem handles. Drain only yields. Actual daemon health cannot claim readiness, durability or graceful drain.
2. Cluster join/leave/peer tracking are not a distributed membership protocol. No demonstrated consensus, fencing, failure-detector guarantees or partition recovery.
3. Handshake authentication is caller policy; supplying auth_token does not automatically validate it. DDAL checksum is corruption detection, not a MAC. Plain TCP does not provide confidentiality/authenticity; applications should configure verified TLS and their auth policy.
4. Frame flags describe capabilities but do not demonstrate integrated compression, fragmentation reassembly, acknowledgement retry/deduplication or priority scheduling.
5. ConnectionPool currently returns an index/metadata handle rather than an exclusive mutable I/O lease; indices can change during eviction. Health checker holds an Arc until explicitly aborted. This needs a future API redesign before calling it a production connection pool.
6. Queue capacities count messages, not aggregate bytes. Application byte admission quotas and overall slow-peer limits are still needed.
7. Registry discovery scores declared capabilities; it does not prove execution capability or enforce healthy/ready-only selection. Federation is not implemented. Capability version parsing currently falls back to 0.0.0 on malformed manifest versions.
8. Shutdown still has no real subsystem drain callbacks; forced flag alone cannot terminate detached arbitrary user tasks.
9. Unix explicit stale socket cleanup remains caller-authorized destructive removal; automatic bind cleanup was removed.

## Decisions

Prioritized fail-closed mTLS, avoiding user file deletion, outbound transport viability, cancellation correctness, truthful health reporting and atomic data structure mutations. Kept changes local to owned crates; no external agent services or unsupported AGI/ASI claims. Broken pre-1.0 handshake API intentionally changed rather than returning a sender that can never work. Endpoint names remain arbitrary strings, now encoded without key ambiguity.

## Validation

`cargo test -p daf-ddal -p daf-transport -p daf-registry -p daf-runtime --lib`: 106 DDAL + 72 registry + 71 runtime + 40 transport = 289 passed, zero failures including the final registry unregister ordering and Unix listener identity hardening. Positive/negative actual TCP mTLS tests also passed independently (`cargo test -p daf-transport tls::tests`: 7 passed). Formatting used rustfmt edition 2024 on changed owned source files. Workspace e2e/doctest/clippy verification remains root-agent responsibility.

## Open questions / next steps

Wire real subsystem ownership, startup failure rollback, probes and bounded drain into Runtime before daemon readiness claims. Replace metadata-only pool handle with exclusive stable leases. Add byte quotas, peer policy, handshake deadlines and version negotiation beyond framed compatibility; integration-test authenticated DDAL traffic end-to-end. Clarify which flags are implemented and which are reserved. Decide healthy/ready discovery policy and validate manifest versions. No evidence supports ASI capability or production distributed reliability.
