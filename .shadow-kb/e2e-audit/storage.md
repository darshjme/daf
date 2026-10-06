# Storage, memory, secrets, infrastructure specialist handoff

## Done
Reviewed daf-memory, daf-logger, daf-vault, daf-provision and daf-configure for specialist domains 15–19. Implemented scoped repairs and regression tests. No commits, Actions, infrastructure creation or deletion performed.

- Provision file locking uses atomic create_new instead of a check/write race. Removed unsafe ten-minute automatic lock takeover. Unlock reports filesystem failures.
- Configuration variables sort layers stably by declared scope, ensuring task values override later-pushed global values in get, merged and templates.
- Secret Debug redacts plaintext-bearing encrypted_value and arbitrary metadata.
- Vault CRUD operations serialize within each store instance, eliminating duplicate-name and read/rotation races. Rotation durably publishes replacement ciphertext/key before removing the prior key. Malformed persisted salt is rejected.
- File logger serializes write/rotation/close, assigns UUID filenames to prevent same-second rotation collisions, calls sync_data before successful writes, and updates size after persistence. Rotation after close errors.
- Memory get and recall hide expired records before periodic compaction.
- Consolidation preserves source agent/conversation and TTL/creation provenance; tags semantic derivations to make repeated distillation idempotent; sorts merge keepers by creation/ID; accumulates counts/tags from current stored keeper; propagates update/delete failures before reporting merge success; prevents merging across distinct ownership or derivation sources; reloads surviving records before reinforcement to avoid stale overwrite; excludes expired records from consolidation.

## Found
Verified bugs above. Architectural gaps remain: VaultStore is a trusted operator API without authenticated caller/policy enforcement; encrypted_value carries plaintext after get and is serializable; retention archive is a rename, not compression, does not enforce max_entries and risks archive-name replacement; retrieval scans all candidates without an indexed/vector backend; consolidation has a 100k review bound and quadratic comparison, approximate Jaccard merging can collapse distinct facts; memory metadata updates are read/modify/write and can race between separate managers; provision/configuration provider operations describe agents/channels rather than managing real infrastructure. No evidence supports ASI or learned cognition claims.

## Validation
Passed cargo test -p daf-vault -p daf-provision -p daf-configure --lib: 51 vault + 79 provision + 90 configure tests at that point.
Passed cargo test -p daf-memory -p daf-logger --lib: 67 memory + 48 logger tests.
After new TTL regression, passed cargo test -p daf-memory --lib: 68 tests.
After new concurrent vault writes / persistent rotation reopen regressions, passed cargo test -p daf-vault --lib store::tests: all 10 selected tests; 43 filtered. Full workspace integration/check remains root-owned. No failure injection or crash simulator was run; durable ordering reviewed and reopen behavior tested.

## Decisions
Preserved existing public interfaces. Explicit stale-lock recovery requires operator intervention; age is not proof of owner death. Vault operation synchronization is within a store instance; persistent sled already excludes independent database opens. Did not claim atomic cross-record memory transactions or authorization the API cannot provide.

## Open questions
Future auth API should accept verified identity/capabilities before decryption, rather than caller-asserted role strings. Approximate consolidation should preserve all raw evidence and use explicit confidence/provenance rather than delete facts based on word overlap. File retention should coordinate with active writer before deletion/archive.

## Next steps
Root integrate tests/docs and final workspace validation. Roadmap: authorization boundary, transaction/CAS memory metadata, indexed bounded retrieval, non-destructive evidence consolidation, genuine provider integrations, retention/archive correctness and crash fault injection.
