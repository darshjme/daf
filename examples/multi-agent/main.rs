//! Multi-agent pipeline — demonstrates spawning multiple agents
//! that collaborate through DDAL channels to process work.
//!
//! This example creates a 3-stage pipeline:
//!
//!   Researcher  -->  Analyzer  -->  Reporter
//!
//! Each agent processes data and hands off context to the next stage
//! via tokio MPSC channels (simulating DDAL transport). The pipeline
//! processes a batch of research topics end-to-end.
//!
//! Key concepts demonstrated:
//! - Multiple agents running concurrently
//! - Message passing between pipeline stages
//! - Conversation/context tracking across handoffs
//! - Coordinated shutdown
//!
//! Run with:
//!   cargo run -p daf-example-multi-agent

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use daf_core::agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
    ResourceLimits,
};
use daf_core::error::{DafError, DafResult};
use daf_core::message::{Message, MessageKind};

// ---------------------------------------------------------------------------
// Pipeline data types — shared between all agents
// ---------------------------------------------------------------------------

/// A research topic that flows through the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResearchTopic {
    /// Unique identifier for tracing this topic through the pipeline.
    conversation_id: String,
    /// The topic to research.
    topic: String,
}

/// Output from the Researcher stage — raw findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResearchFindings {
    conversation_id: String,
    topic: String,
    /// Simulated research results.
    findings: Vec<String>,
    /// Which agent produced these findings.
    researched_by: String,
}

/// Output from the Analyzer stage — structured analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Analysis {
    conversation_id: String,
    topic: String,
    findings_count: usize,
    /// Key insights extracted from the findings.
    insights: Vec<String>,
    /// Risk assessment.
    risk_level: String,
    analyzed_by: String,
}

/// Output from the Reporter stage — final report.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Report {
    conversation_id: String,
    topic: String,
    summary: String,
    recommendation: String,
    reported_by: String,
}

// ---------------------------------------------------------------------------
// ResearcherAgent — Stage 1: gather raw data
// ---------------------------------------------------------------------------

struct ResearcherAgent {
    manifest: AgentManifest,
    status: Arc<Mutex<AgentStatus>>,
    /// Channel to send findings downstream to the Analyzer.
    output_tx: mpsc::Sender<ResearchFindings>,
    /// Channel to receive topics to research.
    input_rx: Arc<Mutex<mpsc::Receiver<ResearchTopic>>>,
}

impl ResearcherAgent {
    fn new(
        input_rx: mpsc::Receiver<ResearchTopic>,
        output_tx: mpsc::Sender<ResearchFindings>,
    ) -> Self {
        let manifest = AgentManifest::new(AgentKind::Specialist, "researcher")
            .with_capability(AgentCapability::new(
                "research",
                "1.0.0",
                "Gathers raw data and findings on a given topic",
            ))
            .with_metadata("stage", "1");

        Self {
            manifest,
            status: Arc::new(Mutex::new(AgentStatus::Spawning)),
            output_tx,
            input_rx: Arc::new(Mutex::new(input_rx)),
        }
    }
}

#[async_trait]
impl Agent for ResearcherAgent {
    async fn initialize(&self, ctx: &AgentContext) -> DafResult<()> {
        info!(agent_id = %ctx.agent_id, "Researcher agent initializing");
        *self.status.lock().await = AgentStatus::Idle;
        Ok(())
    }

    async fn execute(&self, ctx: &AgentContext) -> DafResult<serde_json::Value> {
        *self.status.lock().await = AgentStatus::Executing;
        let mut processed = 0u32;

        // Pull topics from the input channel until it closes
        loop {
            let topic = {
                let mut rx = self.input_rx.lock().await;
                rx.recv().await
            };

            let Some(topic) = topic else {
                info!("Researcher: input channel closed — finishing");
                break;
            };

            info!(
                conversation_id = topic.conversation_id,
                topic = topic.topic,
                "Researcher: investigating topic"
            );

            // Simulate research work
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Produce findings
            let findings = ResearchFindings {
                conversation_id: topic.conversation_id.clone(),
                topic: topic.topic.clone(),
                findings: vec![
                    format!("{} has seen 40% growth in the last quarter", topic.topic),
                    format!("Key competitors in {} space: 3 major players", topic.topic),
                    format!("Market size for {} estimated at $2.1B", topic.topic),
                    format!("Regulatory landscape for {} is evolving", topic.topic),
                ],
                researched_by: self.manifest.id.to_string(),
            };

            // Hand off to the Analyzer via the DDAL channel
            if self.output_tx.send(findings).await.is_err() {
                warn!("Researcher: downstream channel closed unexpectedly");
                break;
            }

            processed += 1;
            info!(
                conversation_id = topic.conversation_id,
                "Researcher: findings sent downstream"
            );
        }

        *self.status.lock().await = AgentStatus::Completed;
        Ok(serde_json::json!({ "topics_researched": processed }))
    }

    async fn handle_message(&self, _ctx: &AgentContext, msg: Message) -> DafResult<()> {
        info!(msg_id = %msg.id, "Researcher: received out-of-band message");
        Ok(())
    }

    async fn shutdown(&self, _ctx: &AgentContext, _timeout: Duration) -> DafResult<()> {
        *self.status.lock().await = AgentStatus::Terminated;
        info!("Researcher agent shut down");
        Ok(())
    }

    async fn health_check(&self) -> DafResult<()> {
        Ok(())
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    fn status(&self) -> AgentStatus {
        self.status.try_lock().map(|s| *s).unwrap_or(AgentStatus::Executing)
    }

    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// AnalyzerAgent — Stage 2: extract insights from raw findings
// ---------------------------------------------------------------------------

struct AnalyzerAgent {
    manifest: AgentManifest,
    status: Arc<Mutex<AgentStatus>>,
    input_rx: Arc<Mutex<mpsc::Receiver<ResearchFindings>>>,
    output_tx: mpsc::Sender<Analysis>,
}

impl AnalyzerAgent {
    fn new(
        input_rx: mpsc::Receiver<ResearchFindings>,
        output_tx: mpsc::Sender<Analysis>,
    ) -> Self {
        let manifest = AgentManifest::new(AgentKind::Specialist, "analyzer")
            .with_capability(AgentCapability::new(
                "analysis",
                "1.0.0",
                "Extracts structured insights from raw research findings",
            ))
            .with_capability(AgentCapability::new(
                "risk_assessment",
                "1.0.0",
                "Evaluates risk levels based on data patterns",
            ))
            .with_metadata("stage", "2");

        Self {
            manifest,
            status: Arc::new(Mutex::new(AgentStatus::Spawning)),
            input_rx: Arc::new(Mutex::new(input_rx)),
            output_tx,
        }
    }
}

#[async_trait]
impl Agent for AnalyzerAgent {
    async fn initialize(&self, ctx: &AgentContext) -> DafResult<()> {
        info!(agent_id = %ctx.agent_id, "Analyzer agent initializing");
        *self.status.lock().await = AgentStatus::Idle;
        Ok(())
    }

    async fn execute(&self, ctx: &AgentContext) -> DafResult<serde_json::Value> {
        *self.status.lock().await = AgentStatus::Executing;
        let mut processed = 0u32;

        loop {
            let findings = {
                let mut rx = self.input_rx.lock().await;
                rx.recv().await
            };

            let Some(findings) = findings else {
                info!("Analyzer: input channel closed — finishing");
                break;
            };

            info!(
                conversation_id = findings.conversation_id,
                findings_count = findings.findings.len(),
                "Analyzer: processing findings"
            );

            // Simulate analysis work
            tokio::time::sleep(Duration::from_millis(150)).await;

            // Determine risk level based on findings content
            let risk_level = if findings.findings.iter().any(|f| f.contains("regulatory")) {
                "medium"
            } else {
                "low"
            };

            let analysis = Analysis {
                conversation_id: findings.conversation_id.clone(),
                topic: findings.topic.clone(),
                findings_count: findings.findings.len(),
                insights: vec![
                    format!("Growth trajectory for {} is positive", findings.topic),
                    format!("Competitive landscape is {}", if findings.findings.len() > 3 { "crowded" } else { "open" }),
                    format!("Market opportunity score: {}/10", findings.findings.len() * 2),
                ],
                risk_level: risk_level.to_string(),
                analyzed_by: self.manifest.id.to_string(),
            };

            if self.output_tx.send(analysis).await.is_err() {
                warn!("Analyzer: downstream channel closed");
                break;
            }

            processed += 1;
            info!(
                conversation_id = findings.conversation_id,
                risk_level,
                "Analyzer: analysis sent downstream"
            );
        }

        *self.status.lock().await = AgentStatus::Completed;
        Ok(serde_json::json!({ "topics_analyzed": processed }))
    }

    async fn handle_message(&self, _ctx: &AgentContext, msg: Message) -> DafResult<()> {
        info!(msg_id = %msg.id, "Analyzer: received out-of-band message");
        Ok(())
    }

    async fn shutdown(&self, _ctx: &AgentContext, _timeout: Duration) -> DafResult<()> {
        *self.status.lock().await = AgentStatus::Terminated;
        info!("Analyzer agent shut down");
        Ok(())
    }

    async fn health_check(&self) -> DafResult<()> {
        Ok(())
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    fn status(&self) -> AgentStatus {
        self.status.try_lock().map(|s| *s).unwrap_or(AgentStatus::Executing)
    }

    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// ReporterAgent — Stage 3: produce final reports
// ---------------------------------------------------------------------------

struct ReporterAgent {
    manifest: AgentManifest,
    status: Arc<Mutex<AgentStatus>>,
    input_rx: Arc<Mutex<mpsc::Receiver<Analysis>>>,
    /// Collects all final reports for the summary.
    reports: Arc<Mutex<Vec<Report>>>,
}

impl ReporterAgent {
    fn new(input_rx: mpsc::Receiver<Analysis>) -> Self {
        let manifest = AgentManifest::new(AgentKind::Specialist, "reporter")
            .with_capability(AgentCapability::new(
                "reporting",
                "1.0.0",
                "Produces human-readable reports from structured analysis",
            ))
            .with_metadata("stage", "3");

        Self {
            manifest,
            status: Arc::new(Mutex::new(AgentStatus::Spawning)),
            input_rx: Arc::new(Mutex::new(input_rx)),
            reports: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Agent for ReporterAgent {
    async fn initialize(&self, ctx: &AgentContext) -> DafResult<()> {
        info!(agent_id = %ctx.agent_id, "Reporter agent initializing");
        *self.status.lock().await = AgentStatus::Idle;
        Ok(())
    }

    async fn execute(&self, ctx: &AgentContext) -> DafResult<serde_json::Value> {
        *self.status.lock().await = AgentStatus::Executing;

        loop {
            let analysis = {
                let mut rx = self.input_rx.lock().await;
                rx.recv().await
            };

            let Some(analysis) = analysis else {
                info!("Reporter: input channel closed — finishing");
                break;
            };

            info!(
                conversation_id = analysis.conversation_id,
                topic = analysis.topic,
                risk = analysis.risk_level,
                "Reporter: generating report"
            );

            // Simulate report generation
            tokio::time::sleep(Duration::from_millis(100)).await;

            let recommendation = match analysis.risk_level.as_str() {
                "low" => "PROCEED — favorable conditions for investment",
                "medium" => "PROCEED WITH CAUTION — monitor regulatory changes",
                "high" => "HOLD — wait for market stabilization",
                _ => "INSUFFICIENT DATA — requires further research",
            };

            let report = Report {
                conversation_id: analysis.conversation_id.clone(),
                topic: analysis.topic.clone(),
                summary: format!(
                    "Analysis of '{}': {} findings processed, {} insights extracted, risk level: {}",
                    analysis.topic,
                    analysis.findings_count,
                    analysis.insights.len(),
                    analysis.risk_level,
                ),
                recommendation: recommendation.to_string(),
                reported_by: self.manifest.id.to_string(),
            };

            info!(
                conversation_id = report.conversation_id,
                recommendation = report.recommendation,
                "Reporter: report complete"
            );

            self.reports.lock().await.push(report);
        }

        let reports = self.reports.lock().await;
        *self.status.lock().await = AgentStatus::Completed;

        Ok(serde_json::json!({
            "reports_generated": reports.len(),
            "reports": *reports,
        }))
    }

    async fn handle_message(&self, _ctx: &AgentContext, msg: Message) -> DafResult<()> {
        info!(msg_id = %msg.id, "Reporter: received out-of-band message");
        Ok(())
    }

    async fn shutdown(&self, _ctx: &AgentContext, _timeout: Duration) -> DafResult<()> {
        *self.status.lock().await = AgentStatus::Terminated;
        info!("Reporter agent shut down");
        Ok(())
    }

    async fn health_check(&self) -> DafResult<()> {
        Ok(())
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    fn status(&self) -> AgentStatus {
        self.status.try_lock().map(|s| *s).unwrap_or(AgentStatus::Executing)
    }

    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// main — wire up the pipeline and run it
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(true)
        .init();

    info!("=== DAF Multi-Agent Pipeline Example ===");
    info!("Pipeline: Researcher --> Analyzer --> Reporter");

    // -----------------------------------------------------------------------
    // 1. Set up DDAL channels between pipeline stages.
    //
    // In production, these would be DDAL binary socket channels managed by
    // the transport layer. Here we use tokio MPSC channels to demonstrate
    // the same data flow pattern.
    // -----------------------------------------------------------------------

    // Channel: Researcher -> Analyzer (carries ResearchFindings)
    let (research_tx, research_rx) = mpsc::channel::<ResearchFindings>(32);

    // Channel: Analyzer -> Reporter (carries Analysis)
    let (analysis_tx, analysis_rx) = mpsc::channel::<Analysis>(32);

    // Channel: main -> Researcher (carries ResearchTopics)
    let (topic_tx, topic_rx) = mpsc::channel::<ResearchTopic>(32);

    // -----------------------------------------------------------------------
    // 2. Create agents for each pipeline stage.
    // -----------------------------------------------------------------------

    let researcher = Arc::new(ResearcherAgent::new(topic_rx, research_tx));
    let analyzer = Arc::new(AnalyzerAgent::new(research_rx, analysis_tx));
    let reporter = Arc::new(ReporterAgent::new(analysis_rx));

    // -----------------------------------------------------------------------
    // 3. Build contexts. All agents share the same session ID so they can
    //    be correlated in logs and traces.
    // -----------------------------------------------------------------------

    let session_id = uuid::Uuid::now_v7();

    let researcher_ctx = AgentContext::new(researcher.manifest().id, session_id);
    let analyzer_ctx = AgentContext::new(analyzer.manifest().id, session_id);
    let reporter_ctx = AgentContext::new(reporter.manifest().id, session_id);

    info!(session = %session_id, "Session created for pipeline");

    // -----------------------------------------------------------------------
    // 4. Initialize all agents concurrently.
    // -----------------------------------------------------------------------

    let (r1, r2, r3) = tokio::join!(
        researcher.initialize(&researcher_ctx),
        analyzer.initialize(&analyzer_ctx),
        reporter.initialize(&reporter_ctx),
    );
    r1?;
    r2?;
    r3?;

    info!("All agents initialized");

    // -----------------------------------------------------------------------
    // 5. Spawn each agent's execute loop as a concurrent task.
    // -----------------------------------------------------------------------

    let researcher_clone = researcher.clone();
    let researcher_ctx_clone = researcher_ctx.clone();
    let researcher_handle = tokio::spawn(async move {
        researcher_clone.execute(&researcher_ctx_clone).await
    });

    let analyzer_clone = analyzer.clone();
    let analyzer_ctx_clone = analyzer_ctx.clone();
    let analyzer_handle = tokio::spawn(async move {
        analyzer_clone.execute(&analyzer_ctx_clone).await
    });

    let reporter_clone = reporter.clone();
    let reporter_ctx_clone = reporter_ctx.clone();
    let reporter_handle = tokio::spawn(async move {
        reporter_clone.execute(&reporter_ctx_clone).await
    });

    // -----------------------------------------------------------------------
    // 6. Feed topics into the pipeline.
    // -----------------------------------------------------------------------

    let topics = vec![
        ResearchTopic {
            conversation_id: "conv-001".into(),
            topic: "AI Infrastructure".into(),
        },
        ResearchTopic {
            conversation_id: "conv-002".into(),
            topic: "Edge Computing".into(),
        },
        ResearchTopic {
            conversation_id: "conv-003".into(),
            topic: "Quantum Networking".into(),
        },
    ];

    info!(count = topics.len(), "Feeding topics into pipeline");

    for topic in topics {
        info!(
            conversation_id = topic.conversation_id,
            topic = topic.topic,
            "Submitting topic"
        );
        topic_tx.send(topic).await?;
    }

    // Close the input channel to signal the Researcher that no more
    // topics are coming. This will cascade through the pipeline:
    // Researcher finishes -> drops research_tx -> Analyzer finishes ->
    // drops analysis_tx -> Reporter finishes.
    drop(topic_tx);
    info!("Input channel closed — pipeline will drain");

    // -----------------------------------------------------------------------
    // 7. Wait for all agents to complete.
    // -----------------------------------------------------------------------

    let researcher_result = researcher_handle.await??;
    info!(result = %researcher_result, "Researcher completed");

    let analyzer_result = analyzer_handle.await??;
    info!(result = %analyzer_result, "Analyzer completed");

    let reporter_result = reporter_handle.await??;
    info!(result = %serde_json::to_string_pretty(&reporter_result)?, "Reporter completed");

    // -----------------------------------------------------------------------
    // 8. Coordinated shutdown.
    // -----------------------------------------------------------------------

    let timeout = Duration::from_secs(5);
    let (s1, s2, s3) = tokio::join!(
        researcher.shutdown(&researcher_ctx, timeout),
        analyzer.shutdown(&analyzer_ctx, timeout),
        reporter.shutdown(&reporter_ctx, timeout),
    );
    s1?;
    s2?;
    s3?;

    info!("=== Pipeline complete — all agents shut down ===");

    Ok(())
}
