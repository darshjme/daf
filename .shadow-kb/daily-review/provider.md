# Provider instruction isolation

## Done
Confirmed the live finding and implemented the mitigation in provider.rs only. Added Unix subprocess regression checking actual argv and cwd.

## Found
Installed Codex CLI --ignore-user-config excludes config.toml but still loads project AGENTS.md. A harmless temporary AGENTS.md containing project marker DAF_DOC_SENTINEL_5f3 was discovered by a baseline exec run (exit 0). The same invocation with --config project_doc_max_bytes=0 returned NO_PROJECT_MARKER (exit 0). No tools or sensitive reads requested. Both calls used gpt-6.1-sol, read-only sandbox and ephemeral mode. Installed exec --help confirms ignore-user-config scope; official configuration reference documents project_doc_max_bytes as maximum AGENTS.md bytes: https://developers.openai.com/codex/config-reference/ .

## Decisions
Run the process itself and Codex -C in the fresh schema temp directory, never the reviewed repository. Set project_doc_max_bytes=0 to exclude parent/global project instruction discovery. Preserve repo cwd field as review identity but declare isolated_working_directory and project_doc_max_bytes explicitly; adapter version bumped to codex-cli-v2 to invalidate prior cache. Resolve explicit relative provider executables before changing process cwd. This is instruction-discovery isolation, not a promise that read-only tools cannot read other host files.

## Validation
cargo test --locked -p daf-review provider::tests --offline: 5 passed, 0 failed. New regression provider asserts -C matches actual process cwd and schema temp directory, differs from fixture repo, and config flag disables documents. cargo fmt --all ran. Two live diagnostic calls reported respectively input/cached/output: 24298/12288/14 and 24230/12288/26. These are separate diagnostic usage, not daily-workflow report totals.

## Open questions
No blocking questions. Remaining tools are still governed by installed CLI sandbox; no blanket filesystem-confidentiality guarantee made.

## Next steps
Parent integrates other specialist fix, runs package validation, repeats real DAF workflow with fresh state (adapter identity changed), updates knowledge report and final outcome.
