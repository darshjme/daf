//! Authenticated, bounded request/reply workers using DDAL over TCP or TLS.
//! Each connection carries one task. Transport acknowledgement follows the
//! executor's durable result; cancellation may leave a durable pending receipt.
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use daf_ddal::{
    codec::DdalCodec,
    protocol::{Frame, FrameType, HEADER_SIZE, MAX_PAYLOAD_SIZE},
};
use daf_sdk::task_types::{SdkTaskResult, SdkTaskSpec};
use daf_transport::{
    connection::Connection,
    tcp::{TcpConnection, TcpTransportConfig},
    tls::{TlsConfig, TlsTransport},
};
use serde::{Deserialize, Serialize};
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinSet};
use tokio_util::codec::{Decoder, Encoder};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("invalid remote configuration: {0}")]
    Configuration(String),
    #[error("remote transport or protocol failed: {0}")]
    Protocol(String),
    #[error("remote request deadline exceeded")]
    Timeout,
    #[error("remote request rejected: {0}")]
    Rejected(String),
}
fn protocol(e: impl std::fmt::Display) -> RemoteError {
    RemoteError::Protocol(e.to_string())
}

/// The SDK task ID is also the durable idempotency ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEnvelope {
    pub task: SdkTaskSpec,
    pub dependencies: Vec<Uuid>,
}
impl TaskEnvelope {
    pub fn new(task: SdkTaskSpec) -> Self {
        Self {
            task,
            dependencies: Vec::new(),
        }
    }
}

/// Maps a credential to a trusted canonical principal; None rejects the request.
/// Implementations must protect credentials at rest and avoid logging them.
pub trait CredentialVerifier: Send + Sync {
    fn verify(&self, credential: &str) -> Option<String>;
    /// Default denies every task until the verifier supplies explicit policy.
    fn authorize(&self, _principal: &str, _task: &SdkTaskSpec) -> bool {
        false
    }
}
/// A single scoped credential stored only as a domain-separated HMAC-SHA256 digest. Provision tokens
/// with sufficient entropy; rotation requires replacing the verifier. No Debug.
pub struct StaticCredential {
    digest: [u8; 32],
    principal: String,
    task_names: Vec<String>,
}
impl StaticCredential {
    pub fn new(
        credential: &str,
        principal: impl Into<String>,
        task_names: Vec<String>,
    ) -> Result<Self, RemoteError> {
        let principal = principal.into();
        if credential.len() < 32 || principal.is_empty() || task_names.is_empty() {
            return Err(RemoteError::Configuration(
                "credential requires >=32 bytes, principal and explicit task names".into(),
            ));
        }
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"DAF remote credential digest v1");
        let digest = ring::hmac::sign(&key, credential.as_bytes());
        Ok(Self {
            digest: digest.as_ref().try_into().expect("SHA256 length"),
            principal,
            task_names,
        })
    }
}
impl CredentialVerifier for StaticCredential {
    fn verify(&self, credential: &str) -> Option<String> {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"DAF remote credential digest v1");
        ring::hmac::verify(&key, credential.as_bytes(), &self.digest)
            .ok()
            .map(|_| self.principal.clone())
    }
    fn authorize(&self, principal: &str, task: &SdkTaskSpec) -> bool {
        principal == self.principal && self.task_names.contains(&task.name)
    }
}
/// Shared durable adapter used by applications and remote workers.
pub struct DurableTaskExecutor {
    ledger: Arc<crate::durable::DurableLedger>,
    agent: Arc<daf_sdk::AgentInstance>,
}
impl DurableTaskExecutor {
    pub fn new(
        ledger: Arc<crate::durable::DurableLedger>,
        agent: Arc<daf_sdk::AgentInstance>,
    ) -> Self {
        Self { ledger, agent }
    }
}
#[async_trait]
impl RemoteTaskExecutor for DurableTaskExecutor {
    async fn execute(&self, principal: &str, task: TaskEnvelope) -> Result<SdkTaskResult, String> {
        self.ledger
            .execute_with_dependencies(principal, task.task, &task.dependencies, &self.agent)
            .await
            .map_err(|e| e.to_string())
    }
}
#[async_trait]
pub trait RemoteTaskExecutor: Send + Sync {
    /// Return only after durable result/effects commit. Resolve dependencies here.
    async fn execute(&self, principal: &str, task: TaskEnvelope) -> Result<SdkTaskResult, String>;
}
/// Observability seam after durable execution and before reply serialization.
#[async_trait]
pub trait ReplyObserver: Send + Sync {
    async fn before_reply(&self, principal: &str, task: &TaskEnvelope, result: &SdkTaskResult);
}
#[derive(Clone, Debug)]
pub struct RemoteWorkerConfig {
    pub bind_address: SocketAddr,
    pub tls: Option<TlsConfig>,
    pub max_sessions: usize,
    /// Includes the DDAL header. Enforced on incoming and outgoing frames.
    pub max_frame_bytes: usize,
    /// Bounds handshake, request, execution, observer and reply together.
    pub session_timeout: Duration,
}
impl Default for RemoteWorkerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:0".parse().expect("literal address"),
            tls: None,
            max_sessions: 16,
            max_frame_bytes: 1024 * 1024,
            session_timeout: Duration::from_secs(30),
        }
    }
}
fn validate(
    addr: SocketAddr,
    tls: &Option<TlsConfig>,
    timeout: Duration,
    cap: usize,
) -> Result<(), RemoteError> {
    if !addr.ip().is_loopback() && tls.is_none() {
        return Err(RemoteError::Configuration(
            "TLS required outside loopback".into(),
        ));
    }
    if timeout.is_zero() || cap <= HEADER_SIZE || cap > HEADER_SIZE + MAX_PAYLOAD_SIZE as usize {
        return Err(RemoteError::Configuration(
            "positive timeout and bounded frame size required".into(),
        ));
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
struct Request {
    credential: String,
    task: TaskEnvelope,
}
#[derive(Serialize, Deserialize)]
struct Response {
    result: Result<SdkTaskResult, String>,
}

pub struct RemoteWorker {
    listener: TcpListener,
    config: RemoteWorkerConfig,
    tls: Option<Arc<TlsTransport>>,
    verifier: Arc<dyn CredentialVerifier>,
    executor: Arc<dyn RemoteTaskExecutor>,
    observer: Option<Arc<dyn ReplyObserver>>,
}
impl RemoteWorker {
    pub async fn bind(
        config: RemoteWorkerConfig,
        verifier: Arc<dyn CredentialVerifier>,
        executor: Arc<dyn RemoteTaskExecutor>,
    ) -> Result<Self, RemoteError> {
        validate(
            config.bind_address,
            &config.tls,
            config.session_timeout,
            config.max_frame_bytes,
        )?;
        if config.max_sessions == 0 {
            return Err(RemoteError::Configuration(
                "max_sessions must be positive".into(),
            ));
        }
        let tls = if let Some(c) = &config.tls {
            let mut t = TlsTransport::new(c.clone()).map_err(protocol)?;
            t.init_server().map_err(protocol)?;
            Some(Arc::new(t))
        } else {
            None
        };
        let listener = TcpListener::bind(config.bind_address)
            .await
            .map_err(protocol)?;
        Ok(Self {
            listener,
            config,
            tls,
            verifier,
            executor,
            observer: None,
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener address")
    }
    pub fn with_observer(mut self, observer: Arc<dyn ReplyObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
    /// Shutdown stops admission and drops all owned session futures. No detached tasks.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), RemoteError> {
        tokio::pin!(shutdown);
        let mut sessions = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _=&mut shutdown=>break,
                _=sessions.join_next(),if !sessions.is_empty()=>{},
                accepted=self.listener.accept(),if sessions.len()<self.config.max_sessions=>{
                    let (stream,peer)=accepted.map_err(protocol)?;
                    let tls=self.tls.clone();let verifier=self.verifier.clone();let executor=self.executor.clone();let observer=self.observer.clone();let cap=self.config.max_frame_bytes;let deadline=self.config.session_timeout;
                    sessions.spawn(async move {
                        let work=async {
                            let mut conn:Box<dyn Connection>=if let Some(t)=tls {Box::new(t.accept_tls(stream).await.map_err(protocol)?)} else {Box::new(TcpConnection::from_stream(stream,peer.to_string()))};
                            let request:Request=read_json(&mut *conn,cap).await?;
                            let result=if let Some(principal)=verifier.verify(&request.credential).filter(|p|!p.is_empty()) {
                                let result=if verifier.authorize(&principal,&request.task.task) {executor.execute(&principal,request.task.clone()).await}else{Err("task authorization rejected".into())};
                                if let (Ok(result),Some(o))=(&result,observer) {o.before_reply(&principal,&request.task,result).await;}
                                result
                            }else{Err("authentication rejected".into())};
                            write_json(&mut *conn,cap,&Response {result}).await?;
                            conn.close().await.map_err(protocol)
                        };
                        let _=tokio::time::timeout(deadline,work).await;
                    });
                }
            }
        }
        sessions.abort_all();
        while sessions.join_next().await.is_some() {}
        Ok(())
    }
}

pub struct RemoteClient {
    address: SocketAddr,
    tls: Option<TlsTransport>,
    timeout: Duration,
    cap: usize,
}
impl RemoteClient {
    pub fn new(
        address: SocketAddr,
        tls: Option<TlsConfig>,
        timeout: Duration,
        max_frame_bytes: usize,
    ) -> Result<Self, RemoteError> {
        validate(address, &tls, timeout, max_frame_bytes)?;
        let tls = if let Some(c) = tls {
            let mut t = TlsTransport::new(c).map_err(protocol)?;
            t.init_client().map_err(protocol)?;
            Some(t)
        } else {
            None
        };
        Ok(Self {
            address,
            tls,
            timeout,
            cap: max_frame_bytes,
        })
    }
    pub async fn submit(
        &self,
        credential: &str,
        task: TaskEnvelope,
    ) -> Result<SdkTaskResult, RemoteError> {
        let task_id = task.task.id;
        let work = async {
            let mut conn: Box<dyn Connection> = if let Some(t) = &self.tls {
                Box::new(
                    t.connect_tls(&self.address.to_string())
                        .await
                        .map_err(protocol)?,
                )
            } else {
                Box::new(
                    TcpConnection::connect(
                        &self.address.to_string(),
                        &TcpTransportConfig {
                            connect_timeout: self.timeout,
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(protocol)?,
                )
            };
            write_json(
                &mut *conn,
                self.cap,
                &Request {
                    credential: credential.into(),
                    task,
                },
            )
            .await?;
            let response: Response = read_json(&mut *conn, self.cap).await?;
            let result = response.result.map_err(RemoteError::Rejected)?;
            if result.task_id != task_id {
                return Err(RemoteError::Protocol("reply task ID mismatch".into()));
            }
            Ok(result)
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| RemoteError::Timeout)?
    }
}
async fn write_json<T: Serialize>(
    conn: &mut dyn Connection,
    cap: usize,
    value: &T,
) -> Result<(), RemoteError> {
    let payload = serde_json::to_vec(value).map_err(protocol)?;
    if payload.len() > cap - HEADER_SIZE {
        return Err(RemoteError::Protocol("frame size exceeded".into()));
    }
    let mut bytes = BytesMut::new();
    DdalCodec::with_max_frame_size(cap)
        .encode(Frame::data(1, Bytes::from(payload)), &mut bytes)
        .map_err(protocol)?;
    conn.write(&bytes).await.map_err(protocol)?;
    conn.flush().await.map_err(protocol)
}
async fn read_json<T: serde::de::DeserializeOwned>(
    conn: &mut dyn Connection,
    cap: usize,
) -> Result<T, RemoteError> {
    let mut codec = DdalCodec::with_max_frame_size(cap);
    let mut bytes = BytesMut::new();
    loop {
        if let Some(frame) = codec.decode(&mut bytes).map_err(protocol)? {
            if frame.frame_type != FrameType::Data || frame.stream_id != 1 || frame.flags != 0 {
                return Err(RemoteError::Protocol("unsupported remote frame".into()));
            }
            return serde_json::from_slice(&frame.payload).map_err(protocol);
        }
        if bytes.len() >= cap {
            return Err(RemoteError::Protocol("frame size exceeded".into()));
        }
        let mut chunk = [0; 4096];
        let room = (cap - bytes.len()).min(chunk.len());
        let n = conn.read(&mut chunk[..room]).await.map_err(protocol)?;
        if n == 0 {
            return Err(RemoteError::Protocol("peer closed before reply".into()));
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Echo(AtomicUsize);
    #[async_trait]
    impl RemoteTaskExecutor for Echo {
        async fn execute(&self, _: &str, task: TaskEnvelope) -> Result<SdkTaskResult, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(SdkTaskResult::success(
                task.task.id,
                task.task.inputs,
                Duration::ZERO,
            ))
        }
    }
    const TOKEN: &str = "public-test-token-not-production-0001";
    fn verifier() -> Arc<dyn CredentialVerifier> {
        Arc::new(StaticCredential::new(TOKEN, "test-principal", vec!["echo".into()]).unwrap())
    }
    async fn launch(
        config: RemoteWorkerConfig,
        executor: Arc<dyn RemoteTaskExecutor>,
    ) -> (
        SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), RemoteError>>,
    ) {
        let worker = RemoteWorker::bind(config, verifier(), executor)
            .await
            .unwrap();
        let addr = worker.local_addr();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(worker.run(async {
            let _ = rx.await;
        }));
        (addr, tx, handle)
    }
    fn client(addr: SocketAddr, tls: Option<TlsConfig>) -> RemoteClient {
        RemoteClient::new(addr, tls, Duration::from_secs(2), 1024 * 1024).unwrap()
    }
    #[tokio::test]
    async fn tcp_auth_scope_and_result() {
        let executor = Arc::new(Echo(AtomicUsize::new(0)));
        let (addr, stop, worker) = launch(Default::default(), executor.clone()).await;
        let c = client(addr, None);
        let task = SdkTaskSpec::new("echo", "test").with_inputs(serde_json::json!({"value":42}));
        let result = c
            .submit(TOKEN, TaskEnvelope::new(task.clone()))
            .await
            .unwrap();
        assert_eq!(result.task_id, task.id);
        assert_eq!(result.output, task.inputs);
        assert!(matches!(
            c.submit("wrong-token", TaskEnvelope::new(task)).await,
            Err(RemoteError::Rejected(_))
        ));
        assert!(matches!(
            c.submit(
                TOKEN,
                TaskEnvelope::new(SdkTaskSpec::new("forbidden", "test"))
            )
            .await,
            Err(RemoteError::Rejected(_))
        ));
        assert_eq!(executor.0.load(Ordering::SeqCst), 1);
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
    }
    #[test]
    fn fail_closed_configuration_and_credentials() {
        assert!(
            RemoteClient::new(
                "192.0.2.1:1234".parse().unwrap(),
                None,
                Duration::from_secs(1),
                1000
            )
            .is_err()
        );
        assert!(
            RemoteClient::new(
                "127.0.0.1:1234".parse().unwrap(),
                None,
                Duration::ZERO,
                1000
            )
            .is_err()
        );
        assert!(StaticCredential::new("short", "principal", vec!["echo".into()]).is_err());
        let v = StaticCredential::new(TOKEN, "canonical", vec!["echo".into()]).unwrap();
        assert_eq!(v.verify(TOKEN).as_deref(), Some("canonical"));
        assert!(v.verify("different").is_none());
    }
    fn tls_fixture() -> TlsConfig {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../daf-transport/tests/fixtures");
        TlsConfig::new(
            base.join("test.pem").to_str().unwrap(),
            base.join("test.key").to_str().unwrap(),
        )
        .with_ca(base.join("ca.pem").to_str().unwrap())
        .with_server_name("localhost")
    }
    #[tokio::test]
    async fn tls_worker_roundtrip() {
        let tls = tls_fixture();
        let config = RemoteWorkerConfig {
            tls: Some(tls.clone()),
            ..Default::default()
        };
        let (addr, stop, worker) = launch(config, Arc::new(Echo(AtomicUsize::new(0)))).await;
        let task = SdkTaskSpec::new("echo", "TLS");
        let id = task.id;
        assert_eq!(
            client(addr, Some(tls))
                .submit(TOKEN, TaskEnvelope::new(task))
                .await
                .unwrap()
                .task_id,
            id
        );
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
    }
    struct Hang(Arc<AtomicUsize>);
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl RemoteTaskExecutor for Hang {
        async fn execute(&self, _: &str, _: TaskEnvelope) -> Result<SdkTaskResult, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let _guard = Guard(self.0.clone());
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn bounded_sessions_cancel_without_leaking_task_futures() {
        let active = Arc::new(AtomicUsize::new(0));
        let (addr, stop, worker) = launch(
            RemoteWorkerConfig {
                max_sessions: 1,
                ..Default::default()
            },
            Arc::new(Hang(active.clone())),
        )
        .await;
        let first = tokio::spawn(async move {
            client(addr, None)
                .submit(TOKEN, TaskEnvelope::new(SdkTaskSpec::new("echo", "hang")))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let second = tokio::spawn(async move {
            client(addr, None)
                .submit(TOKEN, TaskEnvelope::new(SdkTaskSpec::new("echo", "queued")))
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(active.load(Ordering::SeqCst), 1);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(first.await.unwrap().is_err());
        assert!(second.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn oversized_request_never_reaches_executor() {
        let executor = Arc::new(Echo(AtomicUsize::new(0)));
        let (addr, stop, worker) = launch(
            RemoteWorkerConfig {
                max_frame_bytes: 512,
                ..Default::default()
            },
            executor.clone(),
        )
        .await;
        let c = RemoteClient::new(addr, None, Duration::from_secs(1), 512).unwrap();
        assert!(
            c.submit(
                TOKEN,
                TaskEnvelope::new(
                    SdkTaskSpec::new("echo", "x")
                        .with_inputs(serde_json::json!({"oversize":"x".repeat(1000)}))
                )
            )
            .await
            .is_err()
        );
        assert_eq!(executor.0.load(Ordering::SeqCst), 0);
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn malformed_ddal_control_frame_cannot_execute() {
        let executor = Arc::new(Echo(AtomicUsize::new(0)));
        let (addr, stop, worker) = launch(Default::default(), executor.clone()).await;
        let mut conn = TcpConnection::connect(&addr.to_string(), &Default::default())
            .await
            .unwrap();
        let mut bytes = BytesMut::new();
        DdalCodec::new()
            .encode(Frame::control(FrameType::Ping, 1), &mut bytes)
            .unwrap();
        conn.write(&bytes).await.unwrap();
        let mut buf = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), conn.read(&mut buf))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(executor.0.load(Ordering::SeqCst), 0);
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn session_deadline_drops_running_execution() {
        let active = Arc::new(AtomicUsize::new(0));
        let (addr, stop, worker) = launch(
            RemoteWorkerConfig {
                session_timeout: Duration::from_millis(50),
                ..Default::default()
            },
            Arc::new(Hang(active.clone())),
        )
        .await;
        let result = client(addr, None)
            .submit(
                TOKEN,
                TaskEnvelope::new(SdkTaskSpec::new("echo", "deadline")),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(active.load(Ordering::SeqCst), 0);
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
    }
}
