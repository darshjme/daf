# 22 — Credential scope and error handling

## Done
StaticCredential constructor now rejects oversized tokens/principals, blank identities, empty/blank/duplicate/oversized allowlists. Verification bounds token length. Tests establish exact-name and exact-principal scope, case/prefix rejection, default-deny verifier policy, and constructor boundaries. Arbitrary executor Err strings now become generic execution rejected at remote boundary.

## Found
Previously executor errors could reflect handler secrets or storage paths directly to peer. No credentials are logged by these tests; test fixtures are public constants. Scoped authorization covers task name, not arbitrary input semantics.

## Decisions
Suppress internal executor diagnostics in remote rejection; successful/failure SDK result payloads remain caller task data and must not contain secrets. Preserve specific authentication/authorization rejection wording.

## Actual validation
`cargo test --locked -p daf-runtime --quiet`: 86 unit tests + 2 doctests passed, zero failed/ignored. `cargo clippy --locked -p daf-runtime --all-targets --no-deps -- -D warnings`: passed. Final added deadline representability gate: all11focused remote tests passed, followed by final locked strict Clippy and runtime diff check. Primary combines workspace validation. No GitHub Actions, commits, external services or lingering task processes.

## Open questions
Static digest assumes high entropy provisioned credentials; rotation replaces verifier. No role claims, secret vault, rate limiter or semantic input authorization added. Low entropy32-byte strings are not made strong by length alone.

## Next steps
Applications requiring input/resource policy must implement CredentialVerifier::authorize, rather than treating explicit task-name scope as full sandbox authorization.
