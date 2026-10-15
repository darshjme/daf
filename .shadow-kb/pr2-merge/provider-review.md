# Provider and validation review

## Done
Reviewed provider.rs and validation.rs at PR head 73157e6. Independently reproduced one actionable accounting issue against the built Rust library, then fixed it at parent request. No validation defect identified.

## Found
P2: multiple turn.completed receipts were rejected but the last receipt replaced prior usage. An actual library repro with 100/10/20 followed by 200/30/40 returned only 200/30/40, with every field known. This understated usage while permitting the summary to count the attempt as completely known.

## Decisions
Unsupported multiple-completion streams now retain the error but clear all usage counters, with source codex.multiple_completed_turns.unknown. This avoids assuming whether receipts are cumulative or per-turn. A third receipt cannot restore known counters. Terminal completion remains known; accounting is explicitly unknown.

## Validation
cargo test --locked -p daf-review provider::tests --offline: 4 passed, 0 failed. New test covers two and three completion events. cargo fmt --all ran. Only provider.rs implementation changed by this specialist.

## Open questions
None blocking. No claim of exhaustive security assessment or semantic finding correctness.

## Next steps
Parent integrates this fix with other review results, runs scoped package checks, and merges only reviewed head. Live daily-use workflow remains parent-owned.
