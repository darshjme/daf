# Changelog

All notable changes to DAF will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-07

### Added
- Core agent framework with types, traits, and primitives (`daf-core`)
- DDAL binary protocol for inter-agent communication (`daf-ddal`)
- Transport layer: TCP, Unix sockets, TLS, in-process channels (`daf-transport`)
- DAG-based execution engine with wave scheduling (`daf-graph`)
- Structured conversation logging and KB extraction (`daf-logger`)
- Episode-based memory system with 3-tier architecture (`daf-memory`)
- Agent orchestration with missions, sprints, and specialist routing (`daf-orchestrator`)
- Terraform-style agent topology provisioning (`daf-provision`)
- Ansible-style agent configuration management (`daf-configure`)
- Secrets management with envelope encryption (`daf-vault`)
- Agent/plugin registry with capability matching (`daf-registry`)
- Agent development SDK with builder, middleware, templates (`daf-sdk`)
- Runtime bootstrap, signal handling, health monitoring (`daf-runtime`)
- CLI with init, run, agent, provision, configure, log, vault, status commands (`daf-cli`)
- FlatBuffers protocol schema (`proto/ddal.fbs`)
- Benchmarks for DDAL throughput and graph execution
- Integration test suite
- Examples: basic agent, multi-agent pipeline, infrastructure, configuration
