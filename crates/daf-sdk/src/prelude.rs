//! Convenience re-exports for agent developers.
//!
//! Importing `daf_sdk::prelude::*` brings all commonly-used types into
//! scope so that agent code can focus on business logic rather than
//! tracking individual import paths.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! let agent = AgentBuilder::new("my-worker")
//!     .kind(AgentKind::Worker)
//!     .capability("code_review", "1.0.0", "Review code")
//!     .build()?;
//! ```

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

pub use daf_core::agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
    ResourceLimits,
};
pub use daf_core::error::{DafError, DafResult, ErrorContext};
pub use daf_core::message::{Envelope, Message, MessageId, MessageKind, Priority};
pub use daf_core::task::{TaskHandle, TaskId, TaskPriority, TaskResult, TaskSpec, TaskState};
pub use daf_core::resource::{ResourceGuard, ResourceKind, ResourceLimit, ResourcePool};

// ---------------------------------------------------------------------------
// DDAL types
// ---------------------------------------------------------------------------

pub use daf_ddal::protocol::{Frame, FrameFlags, FrameType, ProtocolVersion};
pub use daf_ddal::codec::DdalCodec;

// ---------------------------------------------------------------------------
// SDK types
// ---------------------------------------------------------------------------

// Builder
pub use crate::builder::AgentBuilder;

// Configuration
pub use crate::config::{
    Environment, LoggerConfig, MemoryConfig, SdkConfig, SdkConfigBuilder, TransportConfig,
};

// Handlers
pub use crate::handler::{
    EventHandler, FnMessageHandler, FnTaskHandler, HandlerChain, HandlerRegistry, MessageHandler,
    TaskHandler,
};

// Lifecycle
pub use crate::lifecycle::{AgentInstance, HealthStatus, InstanceState, RuntimeStats};

// Middleware
pub use crate::middleware::{
    AuthMiddleware, LoggingMiddleware, MetricsMiddleware, MetricsSnapshot, Middleware,
    MiddlewareStack, RetryMiddleware, TimeoutMiddleware,
};

// Task / event types
pub use crate::task_types::{Event, SdkTaskResult, SdkTaskSpec};

// Templates
pub use crate::template::{
    MonitorTemplate, PipelineTemplate, RouterTemplate, WorkerTemplate,
};

// Testing (always available, not gated behind `#[cfg(test)]` so integration
// tests in downstream crates can use these utilities).
pub use crate::testing::{
    MockAgent, TestContext, TestHarness,
    test_agent_context, test_message, test_routed_message,
};

// ---------------------------------------------------------------------------
// Common external types frequently needed by agent code
// ---------------------------------------------------------------------------

pub use async_trait::async_trait;
pub use serde::{Deserialize, Serialize};
pub use serde_json::{self, json, Value};
pub use uuid::Uuid;
pub use chrono::{DateTime, Utc};
