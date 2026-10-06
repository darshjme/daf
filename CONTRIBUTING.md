# Contributing to DAF

Read the [engineering standard](docs/STANDARD.md) and [architecture](ARCHITECTURE.md) before changing a subsystem. Capability claims require reproducible evidence. DAF has 14 library/CLI crates, integration tests, benchmarks and executable examples.

Use Rust 2024 (declared minimum 1.85), Cargo, a C/C++ toolchain, CMake and libclang for native RocksDB builds. Run checks locally; GitHub Actions is disabled and must not be created, enabled or dispatched.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo doc --locked --workspace --no-deps
cargo build --locked -p daf
python3 scripts/check_local_cli.py
```

For a smaller iteration, test the changed crate and its actual integration boundary. Tests should prove observable behavior, including rejection, cancellation and persistence failure paths. Keep simulations explicitly labeled. Do not claim throughput, model quality or crash durability without measured evidence and conditions.

Use `thiserror` for library errors, `anyhow` for CLI errors, `tracing` for diagnostics and Tokio for async work. Document public APIs and breaking changes. Avoid logging secret values or authorization tokens. Favor bounded work and explicit unsupported errors over invented success.

Open a branch from `main`. Describe the concrete problem, resulting behavior, validation and remaining limitations in the pull request. Keep third-party notices; project authorship belongs to Darshankumar Joshi. Never rewrite existing history without explicit authorization.

Examples use conventional commit messages such as `fix(daf-sdk): enforce configured middleware before handlers`. Report security concerns privately to darsh@darshj.ai.
