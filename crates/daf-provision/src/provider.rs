//! Provider plugin system for resource lifecycle management.
//!
//! Providers are the bridge between declarative resource specs and the actual
//! creation/management of those resources. Each provider knows how to CRUD
//! one category of resources — analogous to Terraform providers.
//!
//! DAF ships three built-in providers:
//! - [`AgentPoolProvider`]: provisions pools of homogeneous agents
//! - [`ChannelProvider`]: provisions DDAL communication channels
//! - [`PipelineProvider`]: provisions multi-stage task pipelines

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info};
use uuid::Uuid;

use daf_core::DafResult;

// ---------------------------------------------------------------------------
// ProviderSchema
// ---------------------------------------------------------------------------

/// Describes the configuration schema a provider accepts.
///
/// Used by the planner to validate resource configs before attempting any
/// CRUD operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSchema {
    /// Human-readable description of the provider.
    pub description: String,

    /// Map of field name to field descriptor. These are the top-level keys
    /// the provider expects in `ResourceSpec.config`.
    pub fields: HashMap<String, FieldSchema>,
}

/// Schema for a single configuration field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    /// Human-readable description.
    pub description: String,

    /// The JSON type expected (`"string"`, `"number"`, `"boolean"`, `"object"`, `"array"`).
    pub field_type: String,

    /// Whether this field must be present.
    pub required: bool,

    /// Default value if not specified.
    pub default: Option<Value>,
}

impl FieldSchema {
    /// Create a required field.
    pub fn required(description: impl Into<String>, field_type: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            field_type: field_type.into(),
            required: true,
            default: None,
        }
    }

    /// Create an optional field with a default.
    pub fn optional(
        description: impl Into<String>,
        field_type: impl Into<String>,
        default: Value,
    ) -> Self {
        Self {
            description: description.into(),
            field_type: field_type.into(),
            required: false,
            default: Some(default),
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderResult
// ---------------------------------------------------------------------------

/// Result of a provider CRUD operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResult {
    /// Provider-assigned physical ID for the resource.
    pub physical_id: String,

    /// Outputs produced by the operation.
    pub outputs: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// The lifecycle interface every resource provider must implement.
///
/// Providers are registered in the [`ProviderRegistry`] by name and looked
/// up when the applier needs to create, update, or destroy a resource.
#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    /// The provider's unique name (e.g., `"agent_pool"`, `"channel"`).
    fn name(&self) -> &str;

    /// The provider's version string.
    fn version(&self) -> &str;

    /// Return the config schema this provider accepts.
    fn schema(&self) -> ProviderSchema;

    /// Validate a config blob against the schema. Returns a list of
    /// validation errors (empty = valid).
    fn validate(&self, config: &Value) -> Vec<String>;

    /// Create a new resource from the given configuration.
    async fn create(&self, name: &str, config: &Value) -> DafResult<ProviderResult>;

    /// Read the current state of an existing resource.
    async fn read(&self, physical_id: &str) -> DafResult<Option<Value>>;

    /// Update an existing resource in-place.
    async fn update(
        &self,
        physical_id: &str,
        old_config: &Value,
        new_config: &Value,
    ) -> DafResult<ProviderResult>;

    /// Destroy an existing resource.
    async fn delete(&self, physical_id: &str) -> DafResult<()>;

    /// Import an existing resource into DAF management by its external ID.
    async fn import(&self, physical_id: &str) -> DafResult<ProviderResult>;
}

// ---------------------------------------------------------------------------
// AgentPoolProvider
// ---------------------------------------------------------------------------

/// Built-in provider that provisions pools of homogeneous agents.
///
/// Config schema:
/// - `count` (number, required): number of agents in the pool
/// - `kind` (string, optional, default: "worker"): agent kind
/// - `capabilities` (array, optional): list of capability names
/// - `max_memory_bytes` (number, optional): per-agent memory limit
pub struct AgentPoolProvider;

impl AgentPoolProvider {
    /// Create a new agent pool provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentPoolProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Provider for AgentPoolProvider {
    fn name(&self) -> &str {
        "agent_pool"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn schema(&self) -> ProviderSchema {
        let mut fields = HashMap::new();
        fields.insert(
            "count".into(),
            FieldSchema::required("Number of agents in the pool", "number"),
        );
        fields.insert(
            "kind".into(),
            FieldSchema::optional("Agent kind", "string", Value::String("worker".into())),
        );
        fields.insert(
            "capabilities".into(),
            FieldSchema::optional("Capability names", "array", Value::Array(vec![])),
        );
        fields.insert(
            "max_memory_bytes".into(),
            FieldSchema::optional(
                "Per-agent memory limit",
                "number",
                Value::Number(serde_json::Number::from(512 * 1024 * 1024_u64)),
            ),
        );

        ProviderSchema {
            description: "Provisions a pool of homogeneous agents".into(),
            fields,
        }
    }

    fn validate(&self, config: &Value) -> Vec<String> {
        let mut errors = Vec::new();

        match config.get("count") {
            None => errors.push("'count' is required".into()),
            Some(v) if !v.is_number() => errors.push("'count' must be a number".into()),
            Some(v) => {
                if let Some(n) = v.as_u64() {
                    if n == 0 {
                        errors.push("'count' must be > 0".into());
                    }
                }
            }
        }

        if let Some(kind) = config.get("kind") {
            if let Some(s) = kind.as_str() {
                let valid = ["orchestrator", "specialist", "worker", "monitor", "router"];
                if !valid.contains(&s) {
                    errors.push(format!("'kind' must be one of: {}", valid.join(", ")));
                }
            } else {
                errors.push("'kind' must be a string".into());
            }
        }

        errors
    }

    async fn create(&self, name: &str, config: &Value) -> DafResult<ProviderResult> {
        let count = config["count"].as_u64().unwrap_or(1);
        let kind = config
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("worker");

        let pool_id = format!("pool-{}", Uuid::now_v7());

        info!(
            pool_id = %pool_id,
            name = %name,
            count = count,
            kind = %kind,
            "creating agent pool"
        );

        let agent_ids: Vec<Value> = (0..count)
            .map(|_| Value::String(Uuid::now_v7().to_string()))
            .collect();

        let mut outputs = HashMap::new();
        outputs.insert("pool_id".into(), Value::String(pool_id.clone()));
        outputs.insert("agent_ids".into(), Value::Array(agent_ids));
        outputs.insert("count".into(), serde_json::json!(count));
        outputs.insert("kind".into(), Value::String(kind.into()));

        Ok(ProviderResult {
            physical_id: pool_id,
            outputs,
        })
    }

    async fn read(&self, physical_id: &str) -> DafResult<Option<Value>> {
        debug!(physical_id = %physical_id, "reading agent pool");
        // In a real implementation this would query the runtime for pool state.
        Ok(Some(serde_json::json!({
            "physical_id": physical_id,
            "status": "active"
        })))
    }

    async fn update(
        &self,
        physical_id: &str,
        _old_config: &Value,
        new_config: &Value,
    ) -> DafResult<ProviderResult> {
        let count = new_config["count"].as_u64().unwrap_or(1);

        info!(
            physical_id = %physical_id,
            new_count = count,
            "updating agent pool"
        );

        let agent_ids: Vec<Value> = (0..count)
            .map(|_| Value::String(Uuid::now_v7().to_string()))
            .collect();

        let mut outputs = HashMap::new();
        outputs.insert("pool_id".into(), Value::String(physical_id.into()));
        outputs.insert("agent_ids".into(), Value::Array(agent_ids));
        outputs.insert("count".into(), serde_json::json!(count));

        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }

    async fn delete(&self, physical_id: &str) -> DafResult<()> {
        info!(physical_id = %physical_id, "destroying agent pool");
        Ok(())
    }

    async fn import(&self, physical_id: &str) -> DafResult<ProviderResult> {
        let mut outputs = HashMap::new();
        outputs.insert("pool_id".into(), Value::String(physical_id.into()));
        outputs.insert("status".into(), Value::String("imported".into()));

        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }
}

// ---------------------------------------------------------------------------
// ChannelProvider
// ---------------------------------------------------------------------------

/// Built-in provider that provisions DDAL communication channels.
///
/// Config schema:
/// - `from` (string, required): source agent or pool name
/// - `to` (string, required): target agent or pool name
/// - `protocol` (string, optional, default: "direct"): channel protocol
/// - `buffer_size` (number, optional, default: 1024): message buffer size
pub struct ChannelProvider;

impl ChannelProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChannelProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Provider for ChannelProvider {
    fn name(&self) -> &str {
        "channel"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn schema(&self) -> ProviderSchema {
        let mut fields = HashMap::new();
        fields.insert(
            "from".into(),
            FieldSchema::required("Source agent or pool", "string"),
        );
        fields.insert(
            "to".into(),
            FieldSchema::required("Target agent or pool", "string"),
        );
        fields.insert(
            "protocol".into(),
            FieldSchema::optional("Channel protocol", "string", Value::String("direct".into())),
        );
        fields.insert(
            "buffer_size".into(),
            FieldSchema::optional("Message buffer size", "number", Value::Number(1024.into())),
        );

        ProviderSchema {
            description: "Provisions DDAL communication channels between agents".into(),
            fields,
        }
    }

    fn validate(&self, config: &Value) -> Vec<String> {
        let mut errors = Vec::new();

        if config.get("from").and_then(|v| v.as_str()).is_none() {
            errors.push("'from' is required and must be a string".into());
        }
        if config.get("to").and_then(|v| v.as_str()).is_none() {
            errors.push("'to' is required and must be a string".into());
        }

        errors
    }

    async fn create(&self, name: &str, config: &Value) -> DafResult<ProviderResult> {
        let from = config["from"].as_str().unwrap_or("unknown");
        let to = config["to"].as_str().unwrap_or("unknown");
        let protocol = config
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("direct");
        let buffer_size = config
            .get("buffer_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(1024);

        let channel_id = format!("chan-{}", Uuid::now_v7());

        info!(
            channel_id = %channel_id,
            name = %name,
            from = %from,
            to = %to,
            protocol = %protocol,
            "creating channel"
        );

        let mut outputs = HashMap::new();
        outputs.insert("channel_id".into(), Value::String(channel_id.clone()));
        outputs.insert("from".into(), Value::String(from.into()));
        outputs.insert("to".into(), Value::String(to.into()));
        outputs.insert("protocol".into(), Value::String(protocol.into()));
        outputs.insert("buffer_size".into(), serde_json::json!(buffer_size));

        Ok(ProviderResult {
            physical_id: channel_id,
            outputs,
        })
    }

    async fn read(&self, physical_id: &str) -> DafResult<Option<Value>> {
        Ok(Some(serde_json::json!({
            "physical_id": physical_id,
            "status": "active"
        })))
    }

    async fn update(
        &self,
        physical_id: &str,
        _old_config: &Value,
        new_config: &Value,
    ) -> DafResult<ProviderResult> {
        let mut outputs = HashMap::new();
        outputs.insert("channel_id".into(), Value::String(physical_id.into()));
        if let Some(buf) = new_config.get("buffer_size") {
            outputs.insert("buffer_size".into(), buf.clone());
        }

        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }

    async fn delete(&self, physical_id: &str) -> DafResult<()> {
        info!(physical_id = %physical_id, "destroying channel");
        Ok(())
    }

    async fn import(&self, physical_id: &str) -> DafResult<ProviderResult> {
        let mut outputs = HashMap::new();
        outputs.insert("channel_id".into(), Value::String(physical_id.into()));
        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }
}

// ---------------------------------------------------------------------------
// PipelineProvider
// ---------------------------------------------------------------------------

/// Built-in provider that provisions multi-stage task pipelines.
///
/// Config schema:
/// - `stages` (array, required): ordered list of stage objects
///   Each stage: `{ "name": "...", "agent_pool": "...", "parallel": true/false }`
pub struct PipelineProvider;

impl PipelineProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PipelineProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Provider for PipelineProvider {
    fn name(&self) -> &str {
        "pipeline"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn schema(&self) -> ProviderSchema {
        let mut fields = HashMap::new();
        fields.insert(
            "stages".into(),
            FieldSchema::required("Ordered list of pipeline stages", "array"),
        );

        ProviderSchema {
            description: "Provisions ordered multi-stage task pipelines".into(),
            fields,
        }
    }

    fn validate(&self, config: &Value) -> Vec<String> {
        let mut errors = Vec::new();

        match config.get("stages") {
            None => errors.push("'stages' is required".into()),
            Some(v) if !v.is_array() => errors.push("'stages' must be an array".into()),
            Some(v) => {
                let stages = v.as_array().unwrap();
                if stages.is_empty() {
                    errors.push("'stages' must not be empty".into());
                }
                for (i, stage) in stages.iter().enumerate() {
                    if stage.get("name").and_then(|v| v.as_str()).is_none() {
                        errors.push(format!("stage[{i}] must have a 'name' field"));
                    }
                    if stage.get("agent_pool").and_then(|v| v.as_str()).is_none() {
                        errors.push(format!("stage[{i}] must have an 'agent_pool' field"));
                    }
                }
            }
        }

        errors
    }

    async fn create(&self, name: &str, config: &Value) -> DafResult<ProviderResult> {
        let stages = config["stages"].as_array().cloned().unwrap_or_default();
        let pipeline_id = format!("pipe-{}", Uuid::now_v7());

        info!(
            pipeline_id = %pipeline_id,
            name = %name,
            stage_count = stages.len(),
            "creating pipeline"
        );

        let mut outputs = HashMap::new();
        outputs.insert("pipeline_id".into(), Value::String(pipeline_id.clone()));
        outputs.insert("stage_count".into(), serde_json::json!(stages.len()));
        outputs.insert("stages".into(), Value::Array(stages));

        Ok(ProviderResult {
            physical_id: pipeline_id,
            outputs,
        })
    }

    async fn read(&self, physical_id: &str) -> DafResult<Option<Value>> {
        Ok(Some(serde_json::json!({
            "physical_id": physical_id,
            "status": "active"
        })))
    }

    async fn update(
        &self,
        physical_id: &str,
        _old_config: &Value,
        new_config: &Value,
    ) -> DafResult<ProviderResult> {
        let stages = new_config["stages"].as_array().cloned().unwrap_or_default();
        let mut outputs = HashMap::new();
        outputs.insert("pipeline_id".into(), Value::String(physical_id.into()));
        outputs.insert("stage_count".into(), serde_json::json!(stages.len()));
        outputs.insert("stages".into(), Value::Array(stages));

        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }

    async fn delete(&self, physical_id: &str) -> DafResult<()> {
        info!(physical_id = %physical_id, "destroying pipeline");
        Ok(())
    }

    async fn import(&self, physical_id: &str) -> DafResult<ProviderResult> {
        let mut outputs = HashMap::new();
        outputs.insert("pipeline_id".into(), Value::String(physical_id.into()));
        Ok(ProviderResult {
            physical_id: physical_id.into(),
            outputs,
        })
    }
}

// ---------------------------------------------------------------------------
// ProviderRegistry
// ---------------------------------------------------------------------------

/// Registry for provider plugins. Providers are registered by name and
/// resolved during plan/apply when the applier needs to interact with
/// a resource's provider.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Create a registry pre-loaded with the built-in providers.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(AgentPoolProvider::new()));
        reg.register(Arc::new(ChannelProvider::new()));
        reg.register(Arc::new(PipelineProvider::new()));
        reg
    }

    /// Register a provider. Replaces any existing provider with the same name.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        let name = provider.name().to_owned();
        debug!(provider = %name, version = %provider.version(), "registered provider");
        self.providers.insert(name, provider);
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    /// List all registered provider names.
    pub fn list(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Returns the number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Returns `true` if no providers are registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_pool_provider_schema() {
        let p = AgentPoolProvider::new();
        let schema = p.schema();
        assert!(schema.fields.contains_key("count"));
        assert!(schema.fields["count"].required);
    }

    #[test]
    fn agent_pool_provider_validate_valid() {
        let p = AgentPoolProvider::new();
        let config = serde_json::json!({"count": 5, "kind": "worker"});
        let errors = p.validate(&config);
        assert!(errors.is_empty(), "expected no errors, got: {:?}", errors);
    }

    #[test]
    fn agent_pool_provider_validate_missing_count() {
        let p = AgentPoolProvider::new();
        let config = serde_json::json!({"kind": "worker"});
        let errors = p.validate(&config);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("count"));
    }

    #[test]
    fn agent_pool_provider_validate_invalid_kind() {
        let p = AgentPoolProvider::new();
        let config = serde_json::json!({"count": 3, "kind": "invalid"});
        let errors = p.validate(&config);
        assert!(!errors.is_empty());
    }

    #[tokio::test]
    async fn agent_pool_provider_create() {
        let p = AgentPoolProvider::new();
        let config = serde_json::json!({"count": 3, "kind": "specialist"});
        let result = p.create("test-pool", &config).await.unwrap();

        assert!(result.physical_id.starts_with("pool-"));
        assert_eq!(result.outputs["count"], 3);
        assert_eq!(result.outputs["kind"], "specialist");

        let agent_ids = result.outputs["agent_ids"].as_array().unwrap();
        assert_eq!(agent_ids.len(), 3);
    }

    #[tokio::test]
    async fn agent_pool_provider_update() {
        let p = AgentPoolProvider::new();
        let old = serde_json::json!({"count": 3});
        let new = serde_json::json!({"count": 5});
        let result = p.update("pool-123", &old, &new).await.unwrap();

        assert_eq!(result.physical_id, "pool-123");
        let agent_ids = result.outputs["agent_ids"].as_array().unwrap();
        assert_eq!(agent_ids.len(), 5);
    }

    #[tokio::test]
    async fn agent_pool_provider_delete() {
        let p = AgentPoolProvider::new();
        p.delete("pool-123").await.unwrap();
    }

    #[test]
    fn channel_provider_validate_valid() {
        let p = ChannelProvider::new();
        let config = serde_json::json!({"from": "pool-a", "to": "pool-b"});
        let errors = p.validate(&config);
        assert!(errors.is_empty());
    }

    #[test]
    fn channel_provider_validate_missing_fields() {
        let p = ChannelProvider::new();
        let config = serde_json::json!({});
        let errors = p.validate(&config);
        assert_eq!(errors.len(), 2);
    }

    #[tokio::test]
    async fn channel_provider_create() {
        let p = ChannelProvider::new();
        let config = serde_json::json!({
            "from": "researchers",
            "to": "builders",
            "protocol": "pubsub",
            "buffer_size": 2048,
        });
        let result = p.create("research-pipe", &config).await.unwrap();

        assert!(result.physical_id.starts_with("chan-"));
        assert_eq!(result.outputs["from"], "researchers");
        assert_eq!(result.outputs["protocol"], "pubsub");
    }

    #[test]
    fn pipeline_provider_validate_valid() {
        let p = PipelineProvider::new();
        let config = serde_json::json!({
            "stages": [
                {"name": "research", "agent_pool": "researchers"},
                {"name": "build", "agent_pool": "builders"},
            ]
        });
        let errors = p.validate(&config);
        assert!(errors.is_empty());
    }

    #[test]
    fn pipeline_provider_validate_empty_stages() {
        let p = PipelineProvider::new();
        let config = serde_json::json!({"stages": []});
        let errors = p.validate(&config);
        assert!(!errors.is_empty());
    }

    #[tokio::test]
    async fn pipeline_provider_create() {
        let p = PipelineProvider::new();
        let config = serde_json::json!({
            "stages": [
                {"name": "stage1", "agent_pool": "pool-a"},
                {"name": "stage2", "agent_pool": "pool-b"},
            ]
        });
        let result = p.create("etl", &config).await.unwrap();

        assert!(result.physical_id.starts_with("pipe-"));
        assert_eq!(result.outputs["stage_count"], 2);
    }

    #[test]
    fn registry_builtins() {
        let reg = ProviderRegistry::with_builtins();
        assert_eq!(reg.len(), 3);
        assert!(reg.get("agent_pool").is_some());
        assert!(reg.get("channel").is_some());
        assert!(reg.get("pipeline").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn registry_register_custom() {
        let mut reg = ProviderRegistry::new();
        assert!(reg.is_empty());

        reg.register(Arc::new(AgentPoolProvider::new()));
        assert_eq!(reg.len(), 1);

        let names = reg.list();
        assert!(names.contains(&"agent_pool"));
    }

    #[test]
    fn field_schema_constructors() {
        let req = FieldSchema::required("desc", "string");
        assert!(req.required);
        assert!(req.default.is_none());

        let opt = FieldSchema::optional("desc", "number", serde_json::json!(42));
        assert!(!opt.required);
        assert_eq!(opt.default.unwrap(), 42);
    }
}
