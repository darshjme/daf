//! Component acceptance over real TCP. The token gate is a local fixture;
//! this does not establish a production authenticated dispatcher or TLS.
use async_trait::async_trait;
use bytes::Bytes;
use daf_core::{AgentContext, AgentId, DafResult};
use daf_ddal::{
    codec::DdalCodec,
    handshake::{
        HandshakeRequest, HandshakeResponse, complete_handshake_server, perform_handshake_client,
        perform_handshake_server,
    },
    protocol::{Frame, HEADER_SIZE},
};
use daf_memory::{
    store::{MemoryStore, SledStore},
    types::{Memory, MemoryKind},
};
use daf_sdk::{
    handler::TaskHandler,
    task_types::{SdkTaskResult, SdkTaskSpec},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_util::codec::Framed;
use uuid::Uuid;

struct SumHandler(Arc<AtomicUsize>);
#[async_trait]
impl TaskHandler for SumHandler {
    async fn handle_task(&self, task: SdkTaskSpec, _: &AgentContext) -> DafResult<SdkTaskResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let total: i64 = task.inputs["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_i64().unwrap())
            .sum();
        Ok(SdkTaskResult::success(
            task.id,
            json!({"sum": total}),
            Duration::ZERO,
        ))
    }
}

#[tokio::test]
async fn tcp_handshake_handler_result_survives_store_reopen() {
    timeout(Duration::from_secs(5), async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results");
        let store = SledStore::open(&path).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = SumHandler(calls.clone());
        let task = SdkTaskSpec::new("sum", "sum signed inputs")
            .with_inputs(json!({"values": [19, -3, 26]}));
        let task_id = task.id;
        // Both futures are owned here: cancellation and normal return leak no tasks.
        let server = async {
            // Reject the first client before reading any application frame.
            let (mut unauthenticated, _) = listener.accept().await.unwrap();
            let request = perform_handshake_server(&mut unauthenticated)
                .await
                .unwrap();
            assert!(request.auth_token.is_none());
            complete_handshake_server(
                &mut unauthenticated,
                &HandshakeResponse::reject("fixture token required"),
            )
            .await
            .unwrap();
            drop(unauthenticated);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(store.list_all(1).await.unwrap().is_empty());

            let (mut socket, _) = listener.accept().await.unwrap();
            let request = perform_handshake_server(&mut socket).await.unwrap();
            assert_eq!(request.protocol_version, (0, 1, 0));
            assert_eq!(request.auth_token.as_deref(), Some("local-test-token"));
            let session = Uuid::new_v4();
            complete_handshake_server(&mut socket, &HandshakeResponse::accept(session, 7))
                .await
                .unwrap();
            let mut wire = Framed::new(socket, DdalCodec::new());
            let frame = wire.next().await.unwrap().unwrap();
            assert_eq!(frame.stream_id, 7);
            let incoming: SdkTaskSpec = serde_json::from_slice(&frame.payload).unwrap();
            assert_eq!(incoming.id, task_id);
            let result = handler
                .handle_task(incoming, &AgentContext::new(AgentId::new(), session))
                .await
                .unwrap();
            let memory = Memory::new(MemoryKind::Episodic, serde_json::to_value(&result).unwrap());
            store.store(&memory).await.unwrap();
            store.flush().await.unwrap();
            wire.send(Frame::data(
                7,
                Bytes::from(serde_json::to_vec(&result).unwrap()),
            ))
            .await
            .unwrap();
            memory.id
        };
        let client = async {
            let mut socket = TcpStream::connect(address).await.unwrap();
            let rejection = perform_handshake_client(
                &mut socket,
                &HandshakeRequest::new("unauthenticated", "worker"),
            )
            .await;
            assert!(rejection.is_err());
            drop(socket);
            let mut socket = TcpStream::connect(address).await.unwrap();
            let response = perform_handshake_client(
                &mut socket,
                &HandshakeRequest::new("fixture-client", "worker")
                    .with_auth_token("local-test-token"),
            )
            .await
            .unwrap();
            let mut wire = Framed::new(socket, DdalCodec::new());
            wire.send(Frame::data(
                response.assigned_channel_id,
                Bytes::from(serde_json::to_vec(&task).unwrap()),
            ))
            .await
            .unwrap();
            let response = wire.next().await.unwrap().unwrap();
            let result: SdkTaskResult = serde_json::from_slice(&response.payload).unwrap();
            assert!(result.success);
            assert_eq!(result.task_id, task_id);
            assert_eq!(result.output, json!({"sum": 42}));
        };
        let (id, ()) = tokio::join!(server, client);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(store);
        let reopened = SledStore::open(&path).unwrap();
        let persisted = reopened.retrieve(&id).await.unwrap().unwrap();
        let result: SdkTaskResult = serde_json::from_value(persisted.content).unwrap();
        assert_eq!(result.task_id, task_id);
        assert_eq!(result.output, json!({"sum": 42}));
    })
    .await
    .expect("remote pipeline exceeded five seconds");
}

#[tokio::test]
async fn tcp_rejects_corrupt_and_oversized_frames() {
    timeout(Duration::from_secs(5), async {
        for corrupt in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                let mut wire =
                    Framed::new(socket, DdalCodec::with_max_frame_size(HEADER_SIZE + 32));
                assert!(wire.next().await.unwrap().is_err());
            };
            let client = async {
                let mut socket = TcpStream::connect(address).await.unwrap();
                let frame = Frame::data(1, Bytes::from(vec![3; if corrupt { 8 } else { 64 }]));
                let mut encoded = frame.encode_to_bytes().to_vec();
                if corrupt {
                    encoded[HEADER_SIZE] ^= 1;
                }
                socket.write_all(&encoded).await.unwrap();
                socket.shutdown().await.unwrap();
            };
            tokio::join!(server, client);
        }
    })
    .await
    .expect("invalid-frame test exceeded five seconds");
}
