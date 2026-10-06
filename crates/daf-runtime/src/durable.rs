//! Durable execution for **pure SDK preparation handlers**.
//!
//! A handler may run again after a crash before commit. It must prepare declared
//! effects, never perform external side effects. Only ledger effects and the task
//! result commit atomically. Flush completes before a successful acknowledgement.
use std::path::Path;

use daf_sdk::{
    AgentInstance,
    task_types::{SdkTaskResult, SdkTaskSpec},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sled::transaction::{ConflictableTransactionError, TransactionError};
use tokio::sync::Mutex;
use uuid::Uuid;

/// An immutable, principal-scoped transactional effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableEffect {
    pub key: String,
    pub value: Value,
}

/// Explicit preparation contract returned in an SDK handler's result.output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableTaskOutput {
    pub output: Value,
    pub effects: Vec<DurableEffect>,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error("storage error: {0}")]
    Storage(#[from] sled::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("task ID belongs to a different principal or request")]
    Conflict,
    #[error("immutable effect key already exists: {0}")]
    EffectConflict(String),
    #[error("invalid durable task: {0}")]
    Invalid(String),
    #[error("SDK preparation failed: {0}")]
    Handler(String),
}

#[derive(Serialize, Deserialize)]
struct Binding {
    principal: String,
    request: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    principal: String,
    request: Vec<u8>,
    result: SdkTaskResult,
}

/// Local Sled ledger. Sled exclusively locks its directory against a second
/// process. Clones should share this object through Arc; one preparation runs at
/// a time, and no durable "running" record can strand a task after process death.
pub struct DurableLedger {
    db: sled::Db,
    preparation: Mutex<()>,
}

impl DurableLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DurableError> {
        Ok(Self {
            db: sled::open(path)?,
            preparation: Mutex::new(()),
        })
    }

    pub fn lookup(
        &self,
        principal: &str,
        task_id: Uuid,
    ) -> Result<Option<SdkTaskResult>, DurableError> {
        let Some(bytes) = self.db.get(result_key(task_id))? else {
            return Ok(None);
        };
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.principal != principal {
            return Err(DurableError::Conflict);
        }
        Ok(Some(record.result))
    }

    pub fn read_effect(&self, principal: &str, key: &str) -> Result<Option<Value>, DurableError> {
        self.db
            .get(effect_key(principal, key)?)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            .transpose()
    }

    /// Invoke the real SDK handler to prepare effects, then atomically commit
    /// result and immutable effects. Replays bypass the handler entirely.
    /// Admission requires a nonempty authenticated principal supplied by the
    /// transport; this method does not authenticate a caller itself.
    pub async fn execute(
        &self,
        principal: &str,
        task: SdkTaskSpec,
        agent: &AgentInstance,
    ) -> Result<SdkTaskResult, DurableError> {
        self.execute_with_dependencies(principal, task, &[], agent)
            .await
    }

    /// Resolve successful, same-principal predecessors from durable storage.
    /// Inject their outputs under inputs.dependency_results, keyed by UUID.
    pub async fn execute_with_dependencies(
        &self,
        principal: &str,
        mut task: SdkTaskSpec,
        dependencies: &[Uuid],
        agent: &AgentInstance,
    ) -> Result<SdkTaskResult, DurableError> {
        if principal.is_empty() || principal.len() > 4096 || dependencies.len() > 128 {
            return Err(DurableError::Invalid(
                "invalid principal or too many dependencies".into(),
            ));
        }
        // Converting through Value canonically orders labels/object keys.
        let request = serde_json::to_vec(&serde_json::to_value((&task, dependencies))?)?;
        if request.len() > 1_048_576 {
            return Err(DurableError::Invalid("request exceeds 1 MiB".into()));
        }
        let inputs = task
            .inputs
            .as_object_mut()
            .ok_or_else(|| DurableError::Invalid("inputs must be an object".into()))?;
        if inputs.contains_key("dependency_results") {
            return Err(DurableError::Invalid(
                "dependency_results is reserved".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut dependency_results = serde_json::Map::new();
        for id in dependencies {
            if *id == task.id || !seen.insert(*id) {
                return Err(DurableError::Invalid("duplicate or self dependency".into()));
            }
            let result = self
                .lookup(principal, *id)?
                .ok_or_else(|| DurableError::Invalid(format!("missing dependency {id}")))?;
            if !result.success {
                return Err(DurableError::Invalid(format!("failed dependency {id}")));
            }
            dependency_results.insert(id.to_string(), result.output);
            if serde_json::to_vec(&dependency_results)?.len() > 1_048_576 {
                return Err(DurableError::Invalid(
                    "dependency outputs exceed 1 MiB".into(),
                ));
            }
        }
        inputs.insert(
            "dependency_results".into(),
            Value::Object(dependency_results),
        );
        if serde_json::to_vec(&task)?.len() > 1_048_576 {
            return Err(DurableError::Invalid("resolved task exceeds 1 MiB".into()));
        }
        let key = result_key(task.id);
        let _lease = self.preparation.lock().await;
        if let Some(bytes) = self.db.get(&key)? {
            let record: Record = serde_json::from_slice(&bytes)?;
            if record.principal != principal || record.request != request {
                return Err(DurableError::Conflict);
            }
            // A prior task may have committed before a cancelled flush. Never
            // acknowledge even a cache hit before flushing durable state.
            self.db.flush_async().await?;
            return Ok(record.result);
        }
        let binding_key = format!("binding/{}", task.id);
        if let Some(bytes) = self.db.get(&binding_key)? {
            let binding: Binding = serde_json::from_slice(&bytes)?;
            if binding.principal != principal || binding.request != request {
                return Err(DurableError::Conflict);
            }
        } else {
            self.db.insert(
                binding_key.as_bytes(),
                serde_json::to_vec(&Binding {
                    principal: principal.to_owned(),
                    request: request.clone(),
                })?,
            )?;
        }
        self.db.flush_async().await?;
        let mut result = agent
            .process_task(task.clone())
            .await
            .map_err(|e| DurableError::Handler(e.to_string()))?;
        if result.task_id != task.id {
            return Err(DurableError::Invalid(
                "handler returned a different task ID".into(),
            ));
        }
        if serde_json::to_vec(&result)?.len() > 1_048_576 {
            return Err(DurableError::Invalid("result exceeds 1 MiB".into()));
        }
        let effects = if result.success {
            let prepared: DurableTaskOutput = serde_json::from_value(result.output)?;
            result.output = prepared.output;
            prepared.effects
        } else {
            Vec::new()
        };
        if effects.len() > 128 {
            return Err(DurableError::Invalid("more than 128 effects".into()));
        }
        let mut declared = std::collections::HashSet::new();
        let mut writes = Vec::with_capacity(effects.len());
        for effect in effects {
            if effect.key.is_empty()
                || effect.key.len() > 256
                || !declared.insert(effect.key.clone())
            {
                return Err(DurableError::Invalid(
                    "empty or duplicate effect key".into(),
                ));
            }
            writes.push((
                effect_key(principal, &effect.key)?,
                serde_json::to_vec(&effect.value)?,
                effect.key,
            ));
        }
        let record = serde_json::to_vec(&Record {
            principal: principal.to_owned(),
            request,
            result: result.clone(),
        })?;
        self.db
            .transaction(|tree| {
                if tree.get(&key)?.is_some() {
                    return Err(ConflictableTransactionError::Abort(DurableError::Conflict));
                }
                for (effect_key, _, name) in &writes {
                    if tree.get(effect_key)?.is_some() {
                        return Err(ConflictableTransactionError::Abort(
                            DurableError::EffectConflict(name.clone()),
                        ));
                    }
                }
                for (effect_key, value, _) in &writes {
                    tree.insert(effect_key.as_slice(), value.as_slice())?;
                }
                tree.insert(key.as_slice(), record.as_slice())?;
                Ok(())
            })
            .map_err(|e| match e {
                TransactionError::Abort(e) => e,
                TransactionError::Storage(e) => DurableError::Storage(e),
            })?;
        self.db.flush_async().await?;
        Ok(result)
    }
}

fn result_key(id: Uuid) -> Vec<u8> {
    let mut key = b"result/".to_vec();
    key.extend_from_slice(id.as_bytes());
    key
}
fn effect_key(principal: &str, name: &str) -> Result<Vec<u8>, DurableError> {
    let mut key = b"effect/".to_vec();
    key.extend(serde_json::to_vec(&(principal, name))?);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daf_core::{AgentContext, error::DafResult};
    use daf_sdk::{AgentBuilder, handler::TaskHandler};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    struct Prepare(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl TaskHandler for Prepare {
        async fn handle_task(
            &self,
            task: SdkTaskSpec,
            _: &AgentContext,
        ) -> DafResult<SdkTaskResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let output = DurableTaskOutput {
                output: task.inputs.clone(),
                effects: vec![DurableEffect {
                    key: task
                        .inputs
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or("receipt")
                        .into(),
                    value: serde_json::json!({"task": task.id}),
                }],
            };
            Ok(SdkTaskResult::success(
                task.id,
                serde_json::to_value(output).unwrap(),
                Duration::ZERO,
            ))
        }
    }
    #[tokio::test]
    async fn durable_replay_dependencies_conflicts_and_atomic_effects() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = AgentBuilder::new("pure")
            .on_task("prepare", Prepare(calls.clone()))
            .build()
            .unwrap();
        agent.start().await.unwrap();
        let first =
            SdkTaskSpec::new("prepare", "first").with_inputs(serde_json::json!({"key":"one"}));
        {
            let ledger = DurableLedger::open(dir.path()).unwrap();
            ledger
                .execute("alice", first.clone(), &agent)
                .await
                .unwrap();
            let mut changed = first.clone();
            changed.description = "different".into();
            assert!(matches!(
                ledger.execute("alice", changed, &agent).await,
                Err(DurableError::Conflict)
            ));
            assert!(matches!(
                ledger.execute("bob", first.clone(), &agent).await,
                Err(DurableError::Conflict)
            ));
        }
        let ledger = DurableLedger::open(dir.path()).unwrap();
        ledger
            .execute("alice", first.clone(), &agent)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second =
            SdkTaskSpec::new("prepare", "dependent").with_inputs(serde_json::json!({"key":"two"}));
        let result = ledger
            .execute_with_dependencies("alice", second, &[first.id], &agent)
            .await
            .unwrap();
        assert_eq!(
            result.output["dependency_results"][first.id.to_string()]["key"],
            "one"
        );
        let conflicting =
            SdkTaskSpec::new("prepare", "collision").with_inputs(serde_json::json!({"key":"one"}));
        assert!(matches!(
            ledger.execute("alice", conflicting.clone(), &agent).await,
            Err(DurableError::EffectConflict(_))
        ));
        assert!(ledger.lookup("alice", conflicting.id).unwrap().is_none());
        let mut rebound = conflicting.clone();
        rebound.inputs = serde_json::json!({"key":"three"});
        assert!(matches!(
            ledger.execute("alice", rebound, &agent).await,
            Err(DurableError::Conflict)
        ));
        assert!(ledger.read_effect("alice", "three").unwrap().is_none());
        let missing = SdkTaskSpec::new("prepare", "missing");
        assert!(
            ledger
                .execute_with_dependencies("alice", missing, &[Uuid::new_v4()], &agent)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        agent.stop().await.unwrap();
    }
    #[tokio::test]
    async fn interrupted_preparation_retains_request_binding_after_reopen() {
        struct Hanging;
        #[async_trait::async_trait]
        impl TaskHandler for Hanging {
            async fn handle_task(
                &self,
                _: SdkTaskSpec,
                _: &AgentContext,
            ) -> DafResult<SdkTaskResult> {
                std::future::pending().await
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentBuilder::new("hanging")
            .on_task("hang", Hanging)
            .build()
            .unwrap();
        agent.start().await.unwrap();
        let task = SdkTaskSpec::new("hang", "original").with_timeout(Duration::from_millis(10));
        {
            let ledger = DurableLedger::open(dir.path()).unwrap();
            assert!(matches!(
                ledger.execute("alice", task.clone(), &agent).await,
                Err(DurableError::Handler(_))
            ));
            assert!(ledger.lookup("alice", task.id).unwrap().is_none());
        }
        let ledger = DurableLedger::open(dir.path()).unwrap();
        let mut changed = task.clone();
        changed.description = "changed".into();
        assert!(matches!(
            ledger.execute("alice", changed, &agent).await,
            Err(DurableError::Conflict)
        ));
        assert!(matches!(
            ledger.execute("bob", task, &agent).await,
            Err(DurableError::Conflict)
        ));
        agent.stop().await.unwrap();
    }
    #[tokio::test]
    async fn dependency_size_and_principal_gates_run_before_handler() {
        struct Blob(Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl TaskHandler for Blob {
            async fn handle_task(
                &self,
                task: SdkTaskSpec,
                _: &AgentContext,
            ) -> DafResult<SdkTaskResult> {
                self.0.fetch_add(1, Ordering::SeqCst);
                let output = DurableTaskOutput {
                    output: Value::String("x".repeat(600_000)),
                    effects: vec![],
                };
                Ok(SdkTaskResult::success(
                    task.id,
                    serde_json::to_value(output).unwrap(),
                    Duration::ZERO,
                ))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ledger = DurableLedger::open(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = AgentBuilder::new("blob")
            .on_task("blob", Blob(calls.clone()))
            .build()
            .unwrap();
        agent.start().await.unwrap();
        let first = SdkTaskSpec::new("blob", "one");
        let second = SdkTaskSpec::new("blob", "two");
        ledger
            .execute("alice", first.clone(), &agent)
            .await
            .unwrap();
        ledger
            .execute("alice", second.clone(), &agent)
            .await
            .unwrap();
        let dependent = SdkTaskSpec::new("blob", "large dependencies");
        assert!(matches!(
            ledger
                .execute_with_dependencies(
                    "alice",
                    dependent.clone(),
                    &[first.id, second.id],
                    &agent
                )
                .await,
            Err(DurableError::Invalid(_))
        ));
        let expanded = SdkTaskSpec::new("blob", "large resolved input")
            .with_inputs(serde_json::json!({"large":"y".repeat(600_000)}));
        assert!(matches!(
            ledger
                .execute_with_dependencies("alice", expanded, &[first.id], &agent)
                .await,
            Err(DurableError::Invalid(_))
        ));
        assert!(matches!(
            ledger
                .execute_with_dependencies("bob", dependent.clone(), &[first.id], &agent)
                .await,
            Err(DurableError::Conflict)
        ));
        assert!(
            ledger
                .execute_with_dependencies("alice", dependent.clone(), &[first.id; 129], &agent)
                .await
                .is_err()
        );
        assert!(
            ledger
                .execute_with_dependencies(
                    "alice",
                    dependent.clone(),
                    &[first.id, first.id],
                    &agent
                )
                .await
                .is_err()
        );
        assert!(
            ledger
                .execute_with_dependencies("alice", dependent.clone(), &[dependent.id], &agent)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(ledger.lookup("alice", dependent.id).unwrap().is_none());
        agent.stop().await.unwrap();
    }
    #[tokio::test]
    async fn caller_cancellation_and_failed_predecessors_never_unlock_dependents() {
        struct Failure;
        #[async_trait::async_trait]
        impl TaskHandler for Failure {
            async fn handle_task(
                &self,
                task: SdkTaskSpec,
                _: &AgentContext,
            ) -> DafResult<SdkTaskResult> {
                Ok(SdkTaskResult::failure(
                    task.id,
                    "controlled",
                    Duration::ZERO,
                ))
            }
        }
        struct Hanging;
        #[async_trait::async_trait]
        impl TaskHandler for Hanging {
            async fn handle_task(
                &self,
                _: SdkTaskSpec,
                _: &AgentContext,
            ) -> DafResult<SdkTaskResult> {
                std::future::pending().await
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ledger = DurableLedger::open(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = AgentBuilder::new("gated")
            .on_task("fail", Failure)
            .on_task("hang", Hanging)
            .on_task("prepare", Prepare(calls.clone()))
            .build()
            .unwrap();
        agent.start().await.unwrap();
        let failed = SdkTaskSpec::new("fail", "failure");
        assert!(
            !ledger
                .execute("alice", failed.clone(), &agent)
                .await
                .unwrap()
                .success
        );
        let hanging = SdkTaskSpec::new("hang", "cancelled");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                ledger.execute("alice", hanging.clone(), &agent)
            )
            .await
            .is_err()
        );
        assert!(ledger.lookup("alice", hanging.id).unwrap().is_none());
        for predecessor in [failed.id, hanging.id] {
            let dependent = SdkTaskSpec::new("prepare", "must not execute");
            assert!(
                ledger
                    .execute_with_dependencies("alice", dependent.clone(), &[predecessor], &agent)
                    .await
                    .is_err()
            );
            assert!(ledger.lookup("alice", dependent.id).unwrap().is_none());
        }
        // Cancellation released the ledger lease; independent work still runs.
        ledger
            .execute("alice", SdkTaskSpec::new("prepare", "independent"), &agent)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        agent.stop().await.unwrap();
    }
}
