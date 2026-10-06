# Specialist 01 — DDAL wire correctness and allocation admission

## Done

Scoped changes to `crates/daf-ddal/src/protocol.rs` and `codec.rs`; no commits or GitHub Actions.

- Centralized complete-header validation for both decoding entry points. Unknown frame types now reject from their fixed header immediately, before buffering their advertised payload. Header validation errors preserve the input buffer.
- Removed immediate whole-payload reservation on an incomplete frame. A peer advertising the 16 MiB maximum and then stalling no longer makes the codec allocate the advertised payload before those bytes arrive.
- Raw serialization sizes allocation from actual payload bytes, preventing forged public `payload_len = u32::MAX` with a tiny payload from requesting a multi-gigabyte allocation. Documented that this low-level serializer preserves metadata and callers should use DdalCodec for validated writes.
- Codec encode rejects unsupported wire versions and preserves its destination on rejection. Removed a redundant size check.
- Corrected source documentation: fragment flags do not implement reassembly and parsing a handshake does not authenticate a peer.
- Added six regression tests, including every possible header/payload split for a valid frame.

## Found

Unknown type checking previously occurred only after the entire advertised payload was available. Even a valid incomplete header previously caused a reserve of all advertised bytes. These are avoidable memory admission issues on untrusted streams.

`DdalCodec` still intentionally scans through leading garbage to the next magic marker. Flags remain metadata interpreted by the application. Neither resynchronization nor a checksum establishes authentication, encryption, frame delivery or durable execution.

## Decisions

Preserved wire layout, supported type discriminants, flag behavior and existing resynchronization compatibility. Did not redefine pre-1.0 version compatibility or add new dependencies. No throughput, intelligence or production-readiness claims.

## Actual validation

- `cargo test --locked -p daf-ddal --lib`: 112 passed, 0 failed, 0 ignored (106 existing + 6 added).
- `cargo clippy --locked -p daf-ddal --all-targets -- -D warnings`: passed.
- `rustfmt --edition 2024 crates/daf-ddal/src/protocol.rs crates/daf-ddal/src/codec.rs`: completed.
- `git diff --check -- crates/daf-ddal/src/protocol.rs crates/daf-ddal/src/codec.rs`: passed.

These checks ran on the shared current branch. Workspace combined verification remains the parent agent's responsibility after other specialists finish.

## Open questions

An authenticated application's policy may require strict rejection of leading garbage rather than the generic codec's existing resynchronization behavior. This would be an explicit application contract change, not implied by framing. Aggregate connection/byte quotas and request deadlines remain caller responsibilities.

## Next steps

Parent integrates scoped source edits, updates DDAL documentation with early-header/incremental-buffer behavior, and runs final workspace checks. No external services were started and no child process remains from this task.
