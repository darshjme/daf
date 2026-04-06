# Contributing to DAF

Thanks for considering a contribution. DAF is an ambitious project and outside perspective makes it better. This guide covers everything you need to get started.

## Development Environment

### Prerequisites

- **Rust 1.85+** — DAF uses the 2024 edition. Install via [rustup](https://rustup.rs/).
- **RocksDB dependencies** — The cold memory tier uses RocksDB. On macOS: `brew install rocksdb`. On Ubuntu/Debian: `apt install librocksdb-dev`.
- **Git** — for version control (obviously).

### Setup

```bash
# Clone the repo
git clone https://github.com/darshjme/daf.git
cd daf

# Ensure you have the right toolchain
rustup show

# Build everything
cargo build

# Run tests
cargo test

# Run a specific crate's tests
cargo test -p daf-memory
```

### Workspace Structure

DAF is a Cargo workspace with 14 crates under `crates/`:

```
crates/
  daf-core/          # Shared types and traits
  daf-ddal/          # Binary protocol codec
  daf-transport/     # TCP/TLS connections
  daf-memory/        # Three-tier memory
  daf-graph/         # DAG execution engine
  daf-orchestrator/  # Mission lifecycle
  daf-registry/      # Agent discovery
  daf-logger/        # Conversation logging
  daf-provision/     # Declarative provisioning
  daf-configure/     # Playbook configuration
  daf-vault/         # Secret management
  daf-runtime/       # Agent process management
  daf-cli/           # CLI binary
  daf-sdk/           # Public SDK for agent authors
```

## Code Style

### Formatting

All code must pass `cargo fmt`. No exceptions. The project uses default rustfmt settings.

```bash
# Check formatting
cargo fmt --check

# Auto-format
cargo fmt
```

### Linting

All code must pass `cargo clippy` with no warnings.

```bash
# Check lints
cargo clippy --workspace --all-targets -- -D warnings

# Fix auto-fixable lints
cargo clippy --fix
```

### Conventions

- Use `thiserror` for library error types, `anyhow` for application-level errors (CLI).
- Prefer `tracing` over `println!` or `log`.
- All public types and functions need doc comments.
- Use `uuid::Uuid` (v7 for time-ordered IDs, v4 for random IDs).
- Async code uses `tokio`. Do not introduce other async runtimes.
- Prefer `parking_lot::Mutex` over `std::sync::Mutex`.

### Testing

- Unit tests go in the same file as the code, inside `#[cfg(test)] mod tests { ... }`.
- Integration tests go in `tests/`.
- Benchmarks go in `benchmarks/` using Criterion.
- Aim for meaningful tests, not coverage numbers. Test behavior, not implementation.

```bash
# Run all tests
cargo test --workspace

# Run with output
cargo test --workspace -- --nocapture

# Run benchmarks
cargo bench
```

## Pull Request Process

### 1. Fork and Branch

Fork the repo and create a branch from `main`:

```bash
git checkout -b feat/my-feature main
```

Branch naming:
- `feat/description` — new features
- `fix/description` — bug fixes
- `refactor/description` — code restructuring
- `docs/description` — documentation changes
- `test/description` — test additions or fixes

### 2. Make Changes

- Keep commits atomic and well-described.
- Write tests for new functionality.
- Update documentation if you change public APIs.
- Add a note to the relevant crate's CHANGELOG section if applicable.

#### Commit Message Format

Use conventional commits scoped to the crate:

```
feat(daf-memory): add semantic similarity search to hot tier
fix(daf-transport): handle TLS handshake timeout on reconnect
refactor(daf-graph): simplify wave scheduler priority queue
docs(daf-sdk): add middleware chaining example
test(daf-ddal): add roundtrip fuzz tests for frame codec
chore(ci): bump rust-toolchain to 1.86
```

The format is `type(scope): description` where:
- **type** is one of: `feat`, `fix`, `refactor`, `docs`, `test`, `perf`, `chore`, `ci`
- **scope** is the crate name or subsystem (e.g., `daf-core`, `ci`, `deps`)
- **description** is lowercase, imperative mood, no period at the end

### 3. Verify

Before opening a PR, run the full check suite:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

### 4. Open a PR

- Target the `main` branch.
- Write a clear description: what changed, why, and how to test it.
- Link any related issues.
- Keep PRs focused. One concern per PR.

### 5. Review

A maintainer will review your PR. We might ask for changes — that is normal. We review for:
- Correctness
- Performance implications
- API design consistency
- Test coverage for the change
- Documentation

## Crate Ownership

Each crate has a primary maintainer who reviews changes to that crate:

| Crate | Owner |
|-------|-------|
| `daf-core` | @darshjme |
| `daf-ddal` | @darshjme |
| `daf-transport` | @darshjme |
| `daf-memory` | @darshjme |
| `daf-graph` | @darshjme |
| `daf-orchestrator` | @darshjme |
| `daf-registry` | @darshjme |
| `daf-logger` | @darshjme |
| `daf-provision` | @darshjme |
| `daf-configure` | @darshjme |
| `daf-vault` | @darshjme |
| `daf-runtime` | @darshjme |
| `daf-cli` | @darshjme |
| `daf-sdk` | @darshjme |

As contributors take on sustained ownership of crates, this table will be updated.

## Issue Labels

| Label | Meaning |
|-------|---------|
| `bug` | Something is broken |
| `feature` | New functionality request |
| `enhancement` | Improvement to existing functionality |
| `docs` | Documentation only |
| `good-first-issue` | Suitable for newcomers |
| `help-wanted` | Maintainer would appreciate community help |
| `performance` | Related to speed or resource usage |
| `security` | Security-related |
| `breaking` | Will require a semver major bump |

## Priority

Issues are prioritized with labels:

- **P0 — Critical:** Security vulnerabilities, data loss, complete feature breakage. Addressed immediately.
- **P1 — High:** Significant bugs, important features for the next release.
- **P2 — Medium:** Nice-to-have improvements, non-critical bugs.
- **P3 — Low:** Minor improvements, cosmetic issues.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](https://www.contributor-covenant.org/version/2/1/code_of_conduct/). By participating, you agree to uphold a welcoming, inclusive, and harassment-free environment.

Report unacceptable behavior to darsh@darshj.ai.

## Questions?

Open a discussion on GitHub or reach out directly. There are no stupid questions — especially about a codebase this new.
