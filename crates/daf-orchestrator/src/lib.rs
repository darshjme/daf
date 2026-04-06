//! # DAF Orchestrator
//!
//! The brain of the Darshj's Agent Framework — a mission-driven orchestration
//! engine that fuses three proven execution models into one coherent system:
//!
//! 1. **GSD Sprint Methodology** — work is decomposed into time-boxed sprints
//!    with wave-based parallel execution, progress tracking, and retrospectives.
//!
//! 2. **gstack Specialist Routing** — tasks are matched to the right specialist
//!    agent based on capability requirements, load, and context affinity.
//!
//! 3. **Ansible Play/Role Execution** — missions are declarative graphs of
//!    phased tasks with dependency ordering, concurrency limits, and retry
//!    policies.
//!
//! ## Architecture
//!
//! ```text
//!                        ┌─────────────┐
//!                        │ Orchestrator │
//!                        └──────┬──────┘
//!           ┌───────────────────┼───────────────────┐
//!           ▼                   ▼                   ▼
//!    ┌─────────────┐    ┌──────────────┐    ┌───────────────┐
//!    │   Mission    │    │    Sprint    │    │  Specialist   │
//!    │  (playbook)  │    │  (GSD wave)  │    │   Router      │
//!    └──────┬──────┘    └──────┬───────┘    └───────┬───────┘
//!           │                  │                    │
//!           ▼                  ▼                    ▼
//!    ┌─────────────┐    ┌──────────────┐    ┌───────────────┐
//!    │  Supervisor  │    │   Handoff    │    │   Metrics     │
//!    │ (Erlang-ish) │    │  (context)   │    │  (telemetry)  │
//!    └─────────────┘    └──────────────┘    └───────────────┘
//! ```

pub mod handoff;
pub mod metrics;
pub mod mission;
pub mod orchestrator;
pub mod specialist;
pub mod sprint;
pub mod supervisor;

// Re-export the primary public API at crate root.
pub use handoff::{Handoff, HandoffManager, HandoffRecord};
pub use metrics::{AgentMetrics, MetricsCollector, OrchestratorMetrics};
pub use mission::{Mission, MissionId, MissionResult, MissionState, Phase, PhaseResult, RetryPolicy};
pub use orchestrator::{Orchestrator, OrchestratorBuilder, OrchestratorConfig};
pub use specialist::{SpecialistRole, SpecialistRouter};
pub use sprint::{Sprint, SprintId, SprintPlanner, SprintProgress, WaveResult};
pub use supervisor::{DeadLetter, RestartPolicy, Supervisor, SupervisorStrategy};
