# Swarm and CLI verification — 2026-10-06

Created by Darshankumar Joshi. This milestone makes specialist plans executable,
command plans reviewable and DDAL framing and channel shutdown more robust.
The earlier [audit](AUDIT-2026-10-06.md) and
[remote milestone](REMOTE-MILESTONE-2026-10-06.md) remain separate dated evidence.

## Observed results

Local verification used macOS arm64 and Rust/Cargo 1.94.1. Counts describe
executed assertions, not model intelligence, throughput or fleet reliability.

| Check | Actual result |
| --- | --- |
| `cargo test --locked --workspace --all-targets` | 1,019 passed; zero failures or ignored tests |
| `cargo test --locked --workspace --doc` | 9 passed; 15 ignored |
| Strict workspace/all-target Clippy, with `--no-deps -- -D warnings` | Passed; final added DDAL fixtures also passed strict package Clippy |
| Workspace formatting and diff checks | Passed |
| CLI locked build and `scripts/check_local_cli.py target/debug/daf` | 17 checks passed |
| `specialist_plan` example | Three registered handlers completed in dependency order |
| New DDAL golden-wire and TCP fault fixtures | Five passed, included in the workspace total |

The CLI checks start actual processes. They cover successful dependency execution,
invalid/cyclic missions, literal argv, failed dependencies, timeouts, deterministic
JSON, initialization and vault operations. The added preview checks reject invalid
input and confirm that preview causes no command effects. Redirected output has
no decorative banner or ANSI sequences.

The final runtime tests include scoped credentials, cancellation, failed
predecessors, bounded resolved dependency inputs, silent-peer timeouts and shutdown
after durable commit. The existing crash acceptance kills a real worker after
commit and verifies replay and dependency consumption after restart.

## Contracts and limits

`SpecialistPlan` checks plan bytes, assignment count, deadlines, retry limits,
dependency cycles and registered executable capabilities before dispatch. Its
assignment execution is sequential. It neither provisions models nor
automatically binds handler output into later assignments. Preflight does not
reserve workers against later shutdown or removal.

DDAL rejects invalid frame types and incompatible versions from the header,
before waiting for a body. Advertised body size does not trigger eager payload
allocation. Close propagates to shared channel clones, unblocks pending sends
and permits queued messages to drain. A checksum is not peer authentication.

DAF and DJcode's bundled engine share the repaired DDAL contract and identical
golden-wire fixtures. DJcode bundles a smaller core/graph/host snapshot; it does
not expose the entire standalone remote runtime. See [the team guide](SWARM.md).

Thirty numbered assignments are documented under `.shadow-kb/swarm-20261006/`
across the two repositories. The work used three specialists and the primary
agent in waves, within the four-slot execution limit. Those assignments are
technical coverage areas, not a claim of thirty independent human experts.

Live model quality, a distributed fleet, physical terminal interaction and
production deployment were not measured. This work makes no AGI/ASI or arbitrary
external-effect exactly-once claim. GitHub Actions was not used or enabled.
