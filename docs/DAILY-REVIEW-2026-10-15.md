# Daily changed-file review — 2026-10-15

PR [#2](https://github.com/darshjme/daf/pull/2) merged as `73ec975bb6bc0ef833d185929c595f3990c52413` after independent review fixed dangling-symlink admission and ambiguous multiple-completion accounting. Thirty changed-package tests, scoped warning-free clippy, formatting, and seventeen local CLI checks passed. No GitHub Actions ran.

## Real repository run

The merged implementation reviewed its actual twenty-file DAF change against `dafe20d95bb60804c2ac2d4e00f6e833d28d63df`, rather than a seeded fixture. The repository and diff were public. The authenticated Codex provider used `gpt-6.1-sol`, a 240-second per-attempt deadline and zero retries.

```sh
./target/debug/daf review --repo "$PWD" \
  --base dafe20d95bb60804c2ac2d4e00f6e833d28d63df \
  --state-dir ../daily-daf-review-state \
  --model gpt-6.1-sol --timeout-secs 240 --max-retries 0
```

| Specialist | Input tokens | Cached input tokens | Output tokens |
| --- | ---: | ---: | ---: |
| Correctness | 45,010 | 0 | 938 |
| Security | 45,010 | 0 | 341 |
| Total | 90,020 | 0 | 1,279 |

Both attempts succeeded with native usage receipts; no counter was unknown. These are token counts, not a monetary estimate. Repeating the identical command in the restricted environment returned the same decoded JSON report, exactly two attempts and unchanged counters. Completed reviews were reused without new provider calls.

## Finding triage

The correctness specialist identified a snapshot consistency gap. An independent deterministic reproduction deleted an unchanged tracked file after Git emitted its initial changed-name list. The patch included the deletion, but the manifest omitted it and the workflow succeeded. The follow-up binds the manifest into snapshot identity and recollects names before dispatch, rejecting the race before inference.

The security specialist identified repository instruction discovery: setting the reviewed repository as the provider project directory could promote its `AGENTS.md` into instructions. A harmless live sentinel probe confirmed discovery with `--ignore-user-config`; `project_doc_max_bytes=0` suppressed the marker. The provider now uses an isolated temporary process/project directory and disables project documents. Adapter identity v2 prevents reuse of older provider results. This prevents instruction discovery; it does not promise that read-only tools cannot read other host files.

This real run demonstrates useful defect discovery, durable reuse and measured provider usage. It does not establish exhaustive review coverage or guarantee semantic correctness of future findings. Changes after a completed run need a new state directory.

## Follow-up verification

The snapshot race regression asserts rejection before any provider call. The provider subprocess regression verifies its actual working directory and instruction-discovery flag. `cargo test --locked -p daf-review -p daf --all-targets` passed all 32 tests (8 CLI unit, 3 CLI integration, 8 review unit, 13 workflow integration). Scoped clippy, formatting and diff checks passed. The full workspace suite was not rerun.

Two separate live instruction-discovery diagnostic calls reported 48,528 input tokens, 24,576 cached input tokens and 40 output tokens combined. They are not included in the daily workflow's totals above. Cached tokens are a subset of input tokens.

## Corrected workflow live acceptance

The rebuilt adapter reviewed actual public follow-up commit `4327c4d` against merged main `73ec975`, with a fresh `daily-daf-fixed-state` directory and the same model/deadline/retry settings. Both specialists succeeded with no findings. Correctness reported 29,140 input / 0 cached / 152 output tokens; security reported 29,140 / 0 / 103. Totals: 58,280 input, 0 cached and 255 output tokens; no unknown counters. An identical restricted-environment resume produced the same decoded report, exactly two attempts and unchanged usage. This evidence applies to the code at `4327c4d`; the later report-only commit records these results.

Across both real workflow runs and the two instruction-discovery probes, observed usage was 196,828 input tokens, 24,576 cached input tokens and 1,574 output tokens. Resumes added no attempts. These totals exclude the earlier implementation fixture runs documented separately.

The follow-up code and this report are published in [PR #3](https://github.com/darshjme/daf/pull/3). PR #2 is merged; PR #3 remains a separate reviewable follow-up.
