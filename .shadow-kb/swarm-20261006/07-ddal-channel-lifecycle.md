# 07 — DDAL channel lifecycle

## Done
- Replaced independently cloned senders with shared queue ownership and a close token, in source DAF and compatible bundled DJcode DDAL.
- Closing/removing a pooled channel or dropping its pool invalidates stale application handles and cloned inbound writers, wakes sends blocked by full queues, and disconnects a claimed outbound receiver after queued frames drain.
- Keep existing bounded queue/backpressure, channel admission lock, stream mapping and pool capacity semantics. Control sends reject a locally terminal state; shared closure also rejects stale handles whose public state remains Open.
- Added regressions for both directions at capacity, blocked sends, stale delivery/control handles, queued-frame draining/EOF, mapping removal, capacity reuse, and pool-drop teardown.

## Found
Removing the pool entry did not close senders held by existing handles. A claimed outbound receiver could remain connected and stale writers could continue delivering after pool closure. `lifecycle_lock` was already in committed HEAD; verified with `git show HEAD:crates/daf-ddal/src/channel.rs` and preserved in the bundled port.

## Decisions
- Queue admission and close are serialized with a short synchronous lock after asynchronous capacity acquisition. No synchronous lock is held across await.
- Frames already queued before closure are preserved and drained; new frames are rejected. Close does not delete buffered work.
- Preserve public API state fields; pool closure is enforced through shared queue state and may return SendFailed on an otherwise stale Open handle.

## Actual verification
- Source `cargo test --locked -p daf-ddal --lib`: **114 passed, zero failed/ignored**, including the two new lifecycle tests.
- Source `cargo test --locked -p daf-ddal --test wire_contract`: **5 passed**, wire/fault tests added by 27.
- Source `cargo clippy --locked -p daf-ddal --all-targets -- -D warnings`: passed after all source changes.
- Compatible bundled port: **113 library tests passed** and **5 wire-contract tests passed**; bundled strict all-target Clippy passed after equivalent Default derive repairs.
- `cmp` confirms source and bundled channel implementations match exactly. Source scoped diff check and rustfmt passed.

## Open questions
This repairs local bounded-channel teardown, not distributed socket reconnect/replay or cross-host cancellation. Receiver EOF still follows standard polling/drop behavior for outstanding send futures. No network authentication guarantee is introduced by queue closure.

## Next steps
Root integrate with lifecycle consumers and run combined workspace checks. Related DJcode handoffs: 04 workflow preflight/recovery, 16 bundled interoperability, 27 malformed-frame/fault acceptance. Code frozen after verification; no commits or Actions.
