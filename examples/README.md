# Runnable examples

Run these commands from the repository root. All four examples are workspace binary packages using `main.rs` at the package root.

| Command | What runs |
| --- | --- |
| `cargo run -p daf-example-basic-agent` | A local echo agent implementing the DAF `Agent` trait. |
| `cargo run -p daf-example-multi-agent` | Three local agents connected by Tokio MPSC channels, with deterministic research and analysis fixtures. No model inference or network DDAL traffic. |
| `cargo run -p daf-example-infrastructure` | An in-memory topology planning simulation using local example types. No machines or remote agents are provisioned. |
| `cargo run -p daf-example-configuration` | A configuration/playbook simulation using local example types. No remote agents are configured. |

Check all examples without running them:

```sh
cargo check -p daf-example-basic-agent -p daf-example-multi-agent -p daf-example-infrastructure -p daf-example-configuration
```

The infrastructure and configuration examples teach desired-state planning and playbook concepts; their local implementations do not exercise the production provision/configure engines. For integration coverage of actual DDAL framing, handshake, SDK execution and persistent memory over loopback TCP, run `cargo test -p daf-integration-tests --test remote_pipeline`.
