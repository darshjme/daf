# Main verification after PR #3 — 2026-10-15

PR [#3](https://github.com/darshjme/daf/pull/3) merged as `a59de98c9307e88dfe92fe006bd3b6a88dfcdc8b`. Independent premerge review reproduced a relative-PATH regression introduced by provider cwd isolation. Commit `b5e552c0472a0acaeba8295e10848727678f74de` resolves provider executables in the caller context before isolation, sharing lookup with fingerprinting and skipping non-executable Unix candidates.

## Local validation

The exact PR head passed 33 scoped tests: 8 CLI unit, 3 CLI integration, 9 review unit and 13 workflow integration. These cover snapshot races, provider isolation, relative/empty PATH entries, explicit relative executables, interrupted attempts, bounded retries, retained usage and validation. The following completed successfully from a clean detached worktree:

```sh
cargo test --locked -p daf-review -p daf --all-targets
cargo clippy --locked -p daf-review -p daf --all-targets -- -D warnings
cargo fmt --all --check
python3 scripts/check_local_cli.py
git diff --check
```

All 17 local CLI acceptance checks passed. The original checkout's first test attempt failed because an unrelated untracked `review_cli 2.rs` is discovered by Cargo with an invalid crate name. Four pre-existing duplicate files were preserved; clean-worktree validation tests the committed source. The full workspace suite and Windows runtime were not tested. No GitHub Actions ran.

## Actual main run and resume

Built `daf` from branch `main` at the merge commit, then reviewed the eight tracked files changed by PR #3 against the previous main commit. Fresh state, explicit model, zero retries and a 240-second per-attempt timeout were used:

```sh
cargo build --locked -p daf
./target/debug/daf review --repo "$PWD" \
  --base 73ec975bb6bc0ef833d185929c595f3990c52413 \
  --state-dir ../main-pr3-review-state \
  --model gpt-6.1-sol --timeout-secs 240 --max-retries 0
```

| Specialist | Input tokens | Cached input tokens | Output tokens | Findings |
| --- | ---: | ---: | ---: | ---: |
| Correctness | 31,190 | 0 | 405 | 1 |
| Security | 31,190 | 0 | 80 | 0 |
| Total | 62,380 | 0 | 485 | 1 |

Both attempts succeeded with native provider receipts; zero attempts had unknown counters. Repeating the identical command in the restricted environment returned an equal decoded JSON report, exactly the same two attempts and unchanged usage. The resume added no inference attempts. Worker registration messages alone do not indicate a fresh provider call. These totals cover this main run only; they exclude earlier reviews and diagnostic probes.

## Finding and remaining platform work

The live review identified one medium-severity Windows compatibility issue: the resolver searches literal `codex` candidates and does not append `.exe`, unlike Rust's documented executable resolution. The default bare name can therefore fail where only `codex.exe` exists. Source inspection supports the finding; it has not been runtime-reproduced on Windows. [Rust's Command documentation](https://doc.rust-lang.org/std/process/struct.Command.html#method.new) describes the omitted-extension behavior.

This finding remains open after the requested merge and macOS workflow verification. A follow-up should preserve native Windows executable resolution while ensuring fingerprinting and launch choose the same absolute executable, then validate on Windows. An explicit full executable filename avoids this particular lookup omission, but is not evidence of overall Windows support. The live review's success demonstrates operational review, measured usage and durable reuse; it does not mean the reviewed code has no defects.

## Evidence

Local task artifacts retain the original and resumed reports as `outputs/main-pr3-review.json` and `outputs/main-pr3-review-resume.json`, plus the durable journal at `work/main-pr3-review-state/state.json`. The fingerprint below identifies the reviewed snapshot; later knowledge-only changes are not part of this live run.

Snapshot fingerprint: `cb48e16b3ef0ae3a77a5ad80facf22cd891886dd934612cfb5fd2d1eafc83c99`.

- `main-pr3-review.json` SHA-256: `465374decbd9cfcbf35c1f31bb3f34c04eee15b2a7c3bbc9bc25889be8f2ecb3`
- `main-pr3-review-resume.json` SHA-256: `465374decbd9cfcbf35c1f31bb3f34c04eee15b2a7c3bbc9bc25889be8f2ecb3`
