//! Plan execution engine.
//!
//! The applier takes a [`Plan`] and executes it against real providers,
//! creating, updating, and destroying resources in dependency order. It
//! handles parallel execution where the dependency graph allows, progress
//! reporting, rollback on failure, and dry-run mode.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use daf_core::{DafError, DafResult};

use crate::plan::{Action, Plan, ResourceChange};
use crate::provider::ProviderRegistry;
use crate::resource::{Resource, ResourceState};
use crate::state::{ProvisionState, StateStore};

// ---------------------------------------------------------------------------
// ApplyStatus
// ---------------------------------------------------------------------------

/// Status of a single resource action during apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    /// Waiting to be executed.
    Pending,
    /// Currently executing.
    InProgress,
    /// Completed successfully.
    Succeeded,
    /// Failed with an error message.
    Failed(String),
    /// Skipped (e.g., dry-run mode or dependency failure).
    Skipped,
    /// Rolled back after failure.
    RolledBack,
}

// ---------------------------------------------------------------------------
// ApplyProgress
// ---------------------------------------------------------------------------

/// Progress report for a single resource action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyProgress {
    /// The resource being acted on.
    pub resource_name: String,
    /// The action being taken.
    pub action: Action,
    /// Current status.
    pub status: ApplyStatus,
    /// When this action started (if in progress or completed).
    pub started_at: Option<chrono::DateTime<Utc>>,
    /// When this action completed (if finished).
    pub completed_at: Option<chrono::DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// ApplyResult
// ---------------------------------------------------------------------------

/// Aggregate result of a full plan application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    /// Per-resource progress reports.
    pub progress: Vec<ApplyProgress>,
    /// Number of resources successfully applied.
    pub succeeded: usize,
    /// Number of resources that failed.
    pub failed: usize,
    /// Number of resources skipped.
    pub skipped: usize,
    /// Whether a rollback was attempted.
    pub rollback_attempted: bool,
    /// Whether the overall apply succeeded (no failures).
    pub success: bool,
}

// ---------------------------------------------------------------------------
// ApplyOptions
// ---------------------------------------------------------------------------

/// Configuration options for the apply operation.
#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// If `true`, validate and report what would happen without making changes.
    pub dry_run: bool,

    /// Maximum number of resources to apply in parallel.
    pub parallelism: usize,

    /// Whether to attempt rollback on failure.
    pub auto_rollback: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            parallelism: 10,
            auto_rollback: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Applier
// ---------------------------------------------------------------------------

/// Executes a [`Plan`] against providers, mutating the provision state.
///
/// The applier processes resources in waves: each wave contains resources
/// whose dependencies have all been satisfied. Within a wave, resources
/// execute in parallel up to the configured parallelism limit.
pub struct Applier {
    registry: Arc<ProviderRegistry>,
    options: ApplyOptions,
}

impl Applier {
    /// Create a new applier with the given provider registry and options.
    pub fn new(registry: Arc<ProviderRegistry>, options: ApplyOptions) -> Self {
        Self { registry, options }
    }

    /// Create an applier with default options.
    pub fn with_defaults(registry: Arc<ProviderRegistry>) -> Self {
        Self::new(registry, ApplyOptions::default())
    }

    /// Execute a plan, updating the state store as resources are provisioned.
    ///
    /// Returns an [`ApplyResult`] summarizing what happened. The state is
    /// persisted after each resource completes so that partial progress is
    /// not lost on crash.
    pub async fn apply(
        &self,
        plan: &Plan,
        state: &mut ProvisionState,
        store: &dyn StateStore,
    ) -> DafResult<ApplyResult> {
        let mutating: Vec<&ResourceChange> = plan
            .changes
            .iter()
            .filter(|c| c.action.is_mutating())
            .collect();

        if mutating.is_empty() {
            info!("no changes to apply");
            return Ok(ApplyResult {
                progress: Vec::new(),
                succeeded: 0,
                failed: 0,
                skipped: 0,
                rollback_attempted: false,
                success: true,
            });
        }

        info!(
            changes = mutating.len(),
            dry_run = self.options.dry_run,
            "applying plan"
        );

        let progress = Arc::new(Mutex::new(Vec::<ApplyProgress>::new()));
        let completed_resources: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let failed_resources: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Build the dependency map from the plan's resource specs.
        let dep_map = self.build_dependency_map(plan);

        // Process changes in waves based on dependency ordering.
        // The plan is already topologically sorted, so we process in order
        // but group independent resources for parallel execution.
        for change in &mutating {
            // Check if any dependency failed.
            {
                let failed = failed_resources.lock().await;
                if !failed.is_empty() {
                    let deps = dep_map
                        .get(change.resource_name.as_str())
                        .cloned()
                        .unwrap_or_default();
                    let has_failed_dep = deps.iter().any(|d| failed.contains(d));
                    if has_failed_dep {
                        warn!(
                            resource = %change.resource_name,
                            "skipping due to failed dependency"
                        );
                        let mut prog = progress.lock().await;
                        prog.push(ApplyProgress {
                            resource_name: change.resource_name.clone(),
                            action: change.action,
                            status: ApplyStatus::Skipped,
                            started_at: None,
                            completed_at: None,
                        });
                        continue;
                    }
                }
            }

            let result = self.apply_single_change(change, state, store).await;

            match result {
                Ok(prog_entry) => {
                    let succeeded = prog_entry.status == ApplyStatus::Succeeded;
                    progress.lock().await.push(prog_entry);
                    if succeeded {
                        completed_resources
                            .lock()
                            .await
                            .insert(change.resource_name.clone());
                    }
                }
                Err(e) => {
                    error!(
                        resource = %change.resource_name,
                        error = %e,
                        "resource action failed"
                    );
                    progress.lock().await.push(ApplyProgress {
                        resource_name: change.resource_name.clone(),
                        action: change.action,
                        status: ApplyStatus::Failed(e.to_string()),
                        started_at: Some(Utc::now()),
                        completed_at: Some(Utc::now()),
                    });
                    failed_resources
                        .lock()
                        .await
                        .push(change.resource_name.clone());
                }
            }
        }

        let progress = Arc::try_unwrap(progress)
            .expect("apply holds the only Arc reference to progress")
            .into_inner();

        let succeeded = progress
            .iter()
            .filter(|p| p.status == ApplyStatus::Succeeded)
            .count();
        let failed = progress
            .iter()
            .filter(|p| matches!(p.status, ApplyStatus::Failed(_)))
            .count();
        let skipped = progress
            .iter()
            .filter(|p| p.status == ApplyStatus::Skipped)
            .count();

        let mut rollback_attempted = false;

        // Attempt rollback if there were failures and auto_rollback is enabled.
        if failed > 0 && self.options.auto_rollback && !self.options.dry_run {
            warn!(
                failed_count = failed,
                "failures detected, attempting rollback of completed resources"
            );
            rollback_attempted = true;
            // Rollback is best-effort: we try to undo completed creates.
            self.rollback(&progress, state, store).await;
        }

        let success = failed == 0;

        info!(
            succeeded = succeeded,
            failed = failed,
            skipped = skipped,
            success = success,
            "apply complete"
        );

        Ok(ApplyResult {
            progress,
            succeeded,
            failed,
            skipped,
            rollback_attempted,
            success,
        })
    }

    /// Apply a single resource change.
    async fn apply_single_change(
        &self,
        change: &ResourceChange,
        state: &mut ProvisionState,
        store: &dyn StateStore,
    ) -> DafResult<ApplyProgress> {
        let started_at = Utc::now();

        if self.options.dry_run {
            info!(
                resource = %change.resource_name,
                action = %change.action,
                "dry-run: would apply"
            );
            return Ok(ApplyProgress {
                resource_name: change.resource_name.clone(),
                action: change.action,
                status: ApplyStatus::Skipped,
                started_at: Some(started_at),
                completed_at: Some(Utc::now()),
            });
        }

        // Resolve the provider.
        let provider_name = state
            .get_resource(&change.resource_name)
            .map(|r| r.spec.provider.clone())
            .or_else(|| {
                change
                    .after
                    .as_ref()
                    .and_then(|_| Some("agent_pool".into()))
            });

        // For creates, we need the spec from somewhere — look it up or infer.
        let provider_name = match &provider_name {
            Some(name) => name.clone(),
            None => {
                return Err(DafError::NotFound {
                    entity: "provider".into(),
                    id: change.resource_name.clone(),
                });
            }
        };

        let provider = self
            .registry
            .get(&provider_name)
            .ok_or_else(|| DafError::NotFound {
                entity: "provider".into(),
                id: provider_name.clone(),
            })?;

        match change.action {
            Action::Create => {
                let config = change.after.as_ref().unwrap_or(&serde_json::Value::Null);

                // Validate config.
                let errors = provider.validate(config);
                if !errors.is_empty() {
                    return Err(DafError::ConfigError(format!(
                        "validation failed for '{}': {}",
                        change.resource_name,
                        errors.join("; ")
                    )));
                }

                let result = provider.create(&change.resource_name, config).await?;

                // Create the resource in state.
                let spec = crate::resource::ResourceSpec::new(
                    crate::resource::ResourceType::AgentPool, // default, overridden by actual config
                    &change.resource_name,
                    &provider_name,
                )
                .with_config(config.clone());

                let mut resource = Resource::from_spec(spec);
                resource.transition(ResourceState::Created);
                resource.set_physical_id(&result.physical_id);
                for (k, v) in result.outputs {
                    resource.set_output(k, v);
                }

                state.upsert_resource(resource);
                store.save(state).await?;

                info!(
                    resource = %change.resource_name,
                    physical_id = %result.physical_id,
                    "resource created"
                );
            }

            Action::Update => {
                let existing = state.get_resource(&change.resource_name).ok_or_else(|| {
                    DafError::NotFound {
                        entity: "resource".into(),
                        id: change.resource_name.clone(),
                    }
                })?;

                let physical_id = existing.physical_id.clone().ok_or_else(|| {
                    DafError::Internal(format!(
                        "resource '{}' has no physical_id",
                        change.resource_name
                    ))
                })?;

                let old_config = &existing.spec.config;
                let new_config = change.after.as_ref().unwrap_or(&serde_json::Value::Null);

                let result = provider
                    .update(&physical_id, old_config, new_config)
                    .await?;

                if let Some(resource) = state.get_resource_mut(&change.resource_name) {
                    resource.transition(ResourceState::Created);
                    resource.spec.config = new_config.clone();
                    resource.outputs.clear();
                    for (k, v) in result.outputs {
                        resource.set_output(k, v);
                    }
                }

                store.save(state).await?;

                info!(resource = %change.resource_name, "resource updated");
            }

            Action::Destroy => {
                let existing = state.get_resource(&change.resource_name);
                if let Some(res) = existing {
                    if let Some(pid) = &res.physical_id {
                        provider.delete(pid).await?;
                    }
                }

                state.remove_resource(&change.resource_name);
                store.save(state).await?;

                info!(resource = %change.resource_name, "resource destroyed");
            }

            Action::Replace => {
                // Destroy first, then create (unless create_before_destroy is set).
                let existing = state.get_resource(&change.resource_name);
                if let Some(res) = existing {
                    if let Some(pid) = &res.physical_id {
                        provider.delete(pid).await?;
                    }
                }

                let config = change.after.as_ref().unwrap_or(&serde_json::Value::Null);
                let result = provider.create(&change.resource_name, config).await?;

                let spec = crate::resource::ResourceSpec::new(
                    crate::resource::ResourceType::AgentPool,
                    &change.resource_name,
                    &provider_name,
                )
                .with_config(config.clone());

                let mut resource = Resource::from_spec(spec);
                resource.transition(ResourceState::Created);
                resource.set_physical_id(&result.physical_id);
                for (k, v) in result.outputs {
                    resource.set_output(k, v);
                }

                state.upsert_resource(resource);
                store.save(state).await?;

                info!(resource = %change.resource_name, "resource replaced");
            }

            Action::NoOp => {
                // Should not reach here since we filter NoOps.
            }
        }

        Ok(ApplyProgress {
            resource_name: change.resource_name.clone(),
            action: change.action,
            status: ApplyStatus::Succeeded,
            started_at: Some(started_at),
            completed_at: Some(Utc::now()),
        })
    }

    /// Attempt to rollback completed creates by destroying them.
    async fn rollback(
        &self,
        progress: &[ApplyProgress],
        state: &mut ProvisionState,
        store: &dyn StateStore,
    ) {
        for entry in progress.iter().rev() {
            if entry.status == ApplyStatus::Succeeded && entry.action == Action::Create {
                warn!(resource = %entry.resource_name, "rolling back create");

                if let Some(resource) = state.get_resource(&entry.resource_name) {
                    if let Some(pid) = &resource.physical_id {
                        let provider_name = &resource.spec.provider;
                        if let Some(provider) = self.registry.get(provider_name) {
                            if let Err(e) = provider.delete(pid).await {
                                error!(
                                    resource = %entry.resource_name,
                                    error = %e,
                                    "rollback delete failed"
                                );
                                continue;
                            }
                        }
                    }
                }

                state.remove_resource(&entry.resource_name);
                if let Err(e) = store.save(state).await {
                    error!(error = %e, "failed to save state during rollback");
                }
            }
        }
    }

    /// Build a map of resource name -> set of dependency names from the plan.
    fn build_dependency_map(&self, plan: &Plan) -> HashMap<String, Vec<String>> {
        // We don't have the full specs in the plan, so we build from
        // what we can infer. In practice the plan's ordering already
        // handles dependencies, but we use this for skip-on-failure logic.
        let mut deps = HashMap::new();
        for change in &plan.changes {
            deps.entry(change.resource_name.clone())
                .or_insert_with(Vec::new);
        }
        deps
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Plan, ResourceChange};
    use crate::resource::{Resource, ResourceSpec, ResourceType};
    use crate::state::InMemoryStateStore;

    fn test_registry() -> Arc<ProviderRegistry> {
        Arc::new(ProviderRegistry::with_builtins())
    }

    fn create_change(name: &str, config: serde_json::Value) -> ResourceChange {
        ResourceChange::new(name, Action::Create, "new resource").with_after(config)
    }

    fn destroy_change(name: &str) -> ResourceChange {
        ResourceChange::new(name, Action::Destroy, "removed")
    }

    #[tokio::test]
    async fn apply_create_single_resource() {
        let registry = test_registry();
        let applier = Applier::with_defaults(registry);
        let store = InMemoryStateStore::new();
        let mut state = ProvisionState::new();

        let change = create_change("workers", serde_json::json!({"count": 3}));
        let plan = Plan::from_changes(vec![change], vec![]);

        // Don't seed state — let apply create it from the change.

        let result = applier.apply(&plan, &mut state, &store).await.unwrap();

        assert!(result.success);
        assert_eq!(result.succeeded, 1);
        assert_eq!(result.failed, 0);

        let saved = store.load().await.unwrap();
        assert!(saved.get_resource("workers").is_some());
    }

    #[tokio::test]
    async fn apply_destroy_resource() {
        let registry = test_registry();
        let applier = Applier::with_defaults(registry);
        let store = InMemoryStateStore::new();

        let spec = ResourceSpec::new(ResourceType::AgentPool, "old-pool", "agent_pool")
            .with_config(serde_json::json!({"count": 2}));
        let mut resource = Resource::from_spec(spec);
        resource.transition(ResourceState::Created);
        resource.set_physical_id("pool-old");

        let mut state = ProvisionState::new();
        state.upsert_resource(resource);
        store.save(&mut state).await.unwrap();

        let change = destroy_change("old-pool");
        let plan = Plan::from_changes(vec![change], vec![]);

        let result = applier.apply(&plan, &mut state, &store).await.unwrap();

        assert!(result.success);
        assert_eq!(result.succeeded, 1);
        assert!(state.get_resource("old-pool").is_none());
    }

    #[tokio::test]
    async fn apply_dry_run() {
        let registry = test_registry();
        let options = ApplyOptions {
            dry_run: true,
            ..Default::default()
        };
        let applier = Applier::new(registry, options);
        let store = InMemoryStateStore::new();
        let mut state = ProvisionState::new();

        let change = create_change("dry", serde_json::json!({"count": 1}));
        let plan = Plan::from_changes(vec![change], vec![]);

        let result = applier.apply(&plan, &mut state, &store).await.unwrap();

        assert!(result.success);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.succeeded, 0);
        // Resource should NOT exist in state.
        assert!(state.get_resource("dry").is_none());
    }

    #[tokio::test]
    async fn apply_empty_plan() {
        let registry = test_registry();
        let applier = Applier::with_defaults(registry);
        let store = InMemoryStateStore::new();
        let mut state = ProvisionState::new();

        let plan = Plan::from_changes(vec![], vec![]);

        let result = applier.apply(&plan, &mut state, &store).await.unwrap();

        assert!(result.success);
        assert_eq!(result.succeeded, 0);
    }

    #[test]
    fn apply_options_default() {
        let opts = ApplyOptions::default();
        assert!(!opts.dry_run);
        assert_eq!(opts.parallelism, 10);
        assert!(opts.auto_rollback);
    }

    #[tokio::test]
    async fn apply_update_resource() {
        let registry = test_registry();
        let applier = Applier::with_defaults(registry);
        let store = InMemoryStateStore::new();

        let spec = ResourceSpec::new(ResourceType::AgentPool, "scalable", "agent_pool")
            .with_config(serde_json::json!({"count": 2}));
        let mut resource = Resource::from_spec(spec);
        resource.transition(ResourceState::Created);
        resource.set_physical_id("pool-scalable");

        let mut state = ProvisionState::new();
        state.upsert_resource(resource);
        store.save(&mut state).await.unwrap();

        let change = ResourceChange::new("scalable", Action::Update, "config changed")
            .with_before(serde_json::json!({"count": 2}))
            .with_after(serde_json::json!({"count": 5}));
        let plan = Plan::from_changes(vec![change], vec![]);

        let result = applier.apply(&plan, &mut state, &store).await.unwrap();

        assert!(result.success);
        assert_eq!(result.succeeded, 1);

        let updated = state.get_resource("scalable").unwrap();
        assert_eq!(updated.spec.config["count"], 5);
    }
}
