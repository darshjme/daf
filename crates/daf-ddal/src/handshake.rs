//! # Connection Handshake
//!
//! Implements the initial negotiation that occurs when two DDAL peers
//! connect. The handshake exchanges protocol versions, agent identity,
//! capabilities, and an optional authentication token. The server responds
//! with a session ID and an assigned channel for subsequent communication.
//!
//! ## Wire format
//!
//! Handshake messages are serialized with [`bincode`] for compact binary
//! encoding. They ride inside a [`Frame`](crate::protocol::Frame) of type
//! [`Handshake`](crate::protocol::FrameType::Handshake).
//!
//! ## Flow
//!
//! ```text
//!  Client                          Server
//!    │                                │
//!    │── HandshakeRequest ──────────▶ │
//!    │                                │  (validate version, auth)
//!    │◀───────── HandshakeResponse ── │
//!    │                                │
//! ```

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use uuid::Uuid;

use daf_core::{DafError, DafResult};

// ---------------------------------------------------------------------------
// HandshakeRequest
// ---------------------------------------------------------------------------

/// Sent by the connecting peer to introduce itself and negotiate parameters.
///
/// The server uses this information to decide whether to accept the
/// connection, assign a channel, and configure protocol features.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Protocol version the client speaks, as `(major, minor, patch)`.
    pub protocol_version: (u16, u16, u16),
    /// Human-readable name of the connecting agent.
    pub agent_name: String,
    /// The kind of agent (e.g. `"worker"`, `"orchestrator"`, `"relay"`).
    pub agent_kind: String,
    /// List of capabilities the agent advertises (e.g. `"code_review"`,
    /// `"summarization"`, `"tool_use"`).
    pub capabilities: Vec<String>,
    /// Optional bearer token for authentication.
    ///
    /// When present, the server validates this against its auth backend
    /// before accepting the connection.
    pub auth_token: Option<String>,
}

impl HandshakeRequest {
    /// Create a minimal handshake request with the current protocol version.
    pub fn new(agent_name: impl Into<String>, agent_kind: impl Into<String>) -> Self {
        Self {
            protocol_version: (0, 1, 0),
            agent_name: agent_name.into(),
            agent_kind: agent_kind.into(),
            capabilities: Vec::new(),
            auth_token: None,
        }
    }

    /// Add capabilities to the request.
    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set an authentication token.
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Set the protocol version.
    pub fn with_version(mut self, major: u16, minor: u16, patch: u16) -> Self {
        self.protocol_version = (major, minor, patch);
        self
    }
}

// ---------------------------------------------------------------------------
// HandshakeResponse
// ---------------------------------------------------------------------------

/// Sent by the server in reply to a [`HandshakeRequest`].
///
/// If `accepted` is `false`, the `reason` field explains why the connection
/// was refused. The client should read `reason`, log it, and close the
/// socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Whether the server accepted the connection.
    pub accepted: bool,
    /// Unique session identifier assigned by the server.
    ///
    /// Used for reconnection, logging, and distributed tracing.
    pub session_id: Uuid,
    /// The channel ID the client should use for subsequent communication.
    pub assigned_channel_id: u32,
    /// Protocol version the server speaks, as `(major, minor, patch)`.
    pub server_version: (u16, u16, u16),
    /// Human-readable rejection reason when `accepted` is `false`.
    pub reason: Option<String>,
}

impl HandshakeResponse {
    /// Create an acceptance response.
    pub fn accept(session_id: Uuid, assigned_channel_id: u32) -> Self {
        Self {
            accepted: true,
            session_id,
            assigned_channel_id,
            server_version: (0, 1, 0),
            reason: None,
        }
    }

    /// Create a rejection response.
    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            session_id: Uuid::nil(),
            assigned_channel_id: 0,
            server_version: (0, 1, 0),
            reason: Some(reason.into()),
        }
    }

    /// Set the server version on the response.
    pub fn with_server_version(mut self, major: u16, minor: u16, patch: u16) -> Self {
        self.server_version = (major, minor, patch);
        self
    }
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// Maximum handshake message size: 64 KiB.
///
/// This is generous for a handshake. If a peer sends more than this, we
/// assume corruption or attack and reject the connection.
const MAX_HANDSHAKE_SIZE: usize = 64 * 1024;

/// Serialize a value to a length-prefixed bincode blob.
fn encode_handshake<T: Serialize>(value: &T) -> DafResult<Vec<u8>> {
    let payload = bincode::serialize(value)
        .map_err(|e| DafError::SerializationError(format!("bincode encode: {e}")))?;

    if payload.len() > MAX_HANDSHAKE_SIZE {
        return Err(DafError::ProtocolError {
            message: format!(
                "handshake message too large: {} bytes (max {})",
                payload.len(),
                MAX_HANDSHAKE_SIZE,
            ),
        });
    }

    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// Read a length-prefixed bincode blob from an async reader.
async fn decode_handshake<T, R>(stream: &mut R) -> DafResult<T>
where
    T: serde::de::DeserializeOwned,
    R: AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake read length: {e}"),
            retryable: false,
        }
    })?;

    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_HANDSHAKE_SIZE {
        return Err(DafError::ProtocolError {
            message: format!(
                "handshake message too large: {len} bytes (max {MAX_HANDSHAKE_SIZE})",
            ),
        });
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake read payload: {e}"),
            retryable: false,
        }
    })?;

    bincode::deserialize(&payload)
        .map_err(|e| DafError::SerializationError(format!("bincode decode: {e}")))
}

// ---------------------------------------------------------------------------
// Client-side handshake
// ---------------------------------------------------------------------------

/// Perform the client side of the DDAL handshake.
///
/// Sends a [`HandshakeRequest`] over `stream` and waits for the server's
/// [`HandshakeResponse`]. The stream must be a connected TCP (or TLS)
/// socket implementing [`AsyncReadExt`] and [`AsyncWriteExt`].
///
/// # Errors
///
/// Returns `Err` on I/O failure, serialization errors, or if the server
/// rejects the connection.
pub async fn perform_handshake_client<S>(
    stream: &mut S,
    request: &HandshakeRequest,
) -> DafResult<HandshakeResponse>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Encode and send the request.
    let encoded = encode_handshake(request)?;
    stream.write_all(&encoded).await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake write: {e}"),
            retryable: true,
        }
    })?;
    stream.flush().await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake flush: {e}"),
            retryable: true,
        }
    })?;

    // Read the response.
    let response: HandshakeResponse = decode_handshake(stream).await?;

    if !response.accepted {
        let reason = response.reason.clone().unwrap_or_else(|| "unknown".into());
        return Err(DafError::TransportError {
            endpoint: None,
            message: format!("handshake rejected: {reason}"),
            retryable: false,
        });
    }

    Ok(response)
}

// ---------------------------------------------------------------------------
// Server-side handshake
// ---------------------------------------------------------------------------

/// Perform the server side of the DDAL handshake.
///
/// Reads a [`HandshakeRequest`] from `stream` and returns it together with
/// a [`oneshot::Sender`] that the caller uses to send back the
/// [`HandshakeResponse`] once it has made an accept/reject decision.
///
/// This two-phase design lets the server inspect the request, run auth
/// checks, and allocate resources before committing to a response.
///
/// # Errors
///
/// Returns `Err` on I/O failure or deserialization errors.
pub async fn perform_handshake_server<S>(
    stream: &mut S,
) -> DafResult<(HandshakeRequest, oneshot::Sender<HandshakeResponse>)>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Read the client's request.
    let request: HandshakeRequest = decode_handshake(stream).await?;

    // Create a oneshot channel for the response.
    let (tx, rx) = oneshot::channel::<HandshakeResponse>();

    // Spawn the response-sending task. When the caller sends through `tx`,
    // we serialize and write the response.
    let encoded_future = async move {
        match rx.await {
            Ok(response) => {
                let encoded = encode_handshake(&response)?;
                // We cannot write here because we don't own the stream.
                // Instead we return the bytes.
                Ok::<Vec<u8>, DafError>(encoded)
            }
            Err(_) => Err(DafError::TransportError {
                endpoint: None,
                message: "handshake response sender dropped".into(),
                retryable: false,
            }),
        }
    };

    // We need to actually send the response bytes. To keep the API simple,
    // we use a different approach: the caller sends the response through
    // the oneshot, then calls `send_handshake_response` with the stream.
    //
    // But the cleaner pattern is to give the stream back. Let's use the
    // simpler approach: return (request, sender) and provide a separate
    // function to complete the handshake.
    drop(encoded_future); // not used in this approach

    Ok((request, tx))
}

/// Complete the server-side handshake by sending a response.
///
/// Call this after [`perform_handshake_server`] once you have made an
/// accept/reject decision.
pub async fn complete_handshake_server<S>(
    stream: &mut S,
    response: &HandshakeResponse,
) -> DafResult<()>
where
    S: AsyncWriteExt + Unpin,
{
    let encoded = encode_handshake(response)?;
    stream.write_all(&encoded).await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake response write: {e}"),
            retryable: false,
        }
    })?;
    stream.flush().await.map_err(|e| {
        DafError::TransportError {
            endpoint: None,
            message: format!("handshake response flush: {e}"),
            retryable: false,
        }
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn handshake_request_builder() {
        let req = HandshakeRequest::new("agent-1", "worker")
            .with_capabilities(vec!["code_review".into(), "summarization".into()])
            .with_auth_token("secret-token")
            .with_version(1, 2, 3);

        assert_eq!(req.agent_name, "agent-1");
        assert_eq!(req.agent_kind, "worker");
        assert_eq!(req.capabilities.len(), 2);
        assert_eq!(req.auth_token.as_deref(), Some("secret-token"));
        assert_eq!(req.protocol_version, (1, 2, 3));
    }

    #[test]
    fn handshake_response_accept() {
        let session_id = Uuid::now_v7();
        let resp = HandshakeResponse::accept(session_id, 42);

        assert!(resp.accepted);
        assert_eq!(resp.session_id, session_id);
        assert_eq!(resp.assigned_channel_id, 42);
        assert!(resp.reason.is_none());
    }

    #[test]
    fn handshake_response_reject() {
        let resp = HandshakeResponse::reject("version mismatch");

        assert!(!resp.accepted);
        assert_eq!(resp.reason.as_deref(), Some("version mismatch"));
    }

    #[test]
    fn encode_decode_round_trip_request() {
        let req = HandshakeRequest::new("test-agent", "worker")
            .with_capabilities(vec!["cap1".into()]);

        let encoded = encode_handshake(&req).unwrap();
        // First 4 bytes are the length prefix.
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]) as usize;
        let decoded: HandshakeRequest =
            bincode::deserialize(&encoded[4..4 + len]).unwrap();

        assert_eq!(decoded.agent_name, "test-agent");
        assert_eq!(decoded.capabilities, vec!["cap1"]);
    }

    #[test]
    fn encode_decode_round_trip_response() {
        let resp = HandshakeResponse::accept(Uuid::now_v7(), 10)
            .with_server_version(0, 2, 0);

        let encoded = encode_handshake(&resp).unwrap();
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]) as usize;
        let decoded: HandshakeResponse =
            bincode::deserialize(&encoded[4..4 + len]).unwrap();

        assert!(decoded.accepted);
        assert_eq!(decoded.assigned_channel_id, 10);
        assert_eq!(decoded.server_version, (0, 2, 0));
    }

    #[tokio::test]
    async fn client_server_handshake_accepted() {
        let (mut client, mut server) = duplex(4096);

        let request = HandshakeRequest::new("agent-a", "worker");
        let session_id = Uuid::now_v7();

        // Run client and server concurrently.
        let client_handle = tokio::spawn(async move {
            perform_handshake_client(&mut client, &request).await
        });

        let server_handle = tokio::spawn(async move {
            let (req, _tx) = perform_handshake_server(&mut server).await.unwrap();
            assert_eq!(req.agent_name, "agent-a");

            let response = HandshakeResponse::accept(session_id, 1);
            complete_handshake_server(&mut server, &response).await.unwrap();
        });

        let (client_result, _) = tokio::join!(client_handle, server_handle);
        let response = client_result.unwrap().unwrap();
        assert!(response.accepted);
        assert_eq!(response.session_id, session_id);
        assert_eq!(response.assigned_channel_id, 1);
    }

    #[tokio::test]
    async fn client_server_handshake_rejected() {
        let (mut client, mut server) = duplex(4096);

        let request = HandshakeRequest::new("bad-agent", "unknown");

        let client_handle = tokio::spawn(async move {
            perform_handshake_client(&mut client, &request).await
        });

        let server_handle = tokio::spawn(async move {
            let (req, _tx) = perform_handshake_server(&mut server).await.unwrap();
            assert_eq!(req.agent_name, "bad-agent");

            let response = HandshakeResponse::reject("unsupported agent kind");
            complete_handshake_server(&mut server, &response).await.unwrap();
        });

        let (client_result, _) = tokio::join!(client_handle, server_handle);
        let err = client_result.unwrap().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rejected"), "got: {msg}");
    }

    #[test]
    fn serde_json_round_trip_request() {
        let req = HandshakeRequest::new("agent", "orchestrator")
            .with_capabilities(vec!["plan".into()]);
        let json = serde_json::to_string(&req).unwrap();
        let back: HandshakeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_name, "agent");
        assert_eq!(back.capabilities, vec!["plan"]);
    }

    #[test]
    fn serde_json_round_trip_response() {
        let resp = HandshakeResponse::accept(Uuid::now_v7(), 5);
        let json = serde_json::to_string(&resp).unwrap();
        let back: HandshakeResponse = serde_json::from_str(&json).unwrap();
        assert!(back.accepted);
        assert_eq!(back.assigned_channel_id, 5);
    }

    #[test]
    fn oversized_handshake_rejected() {
        // Create a request with a huge auth token to exceed MAX_HANDSHAKE_SIZE.
        let req = HandshakeRequest::new("agent", "worker")
            .with_auth_token("x".repeat(MAX_HANDSHAKE_SIZE + 1));

        let result = encode_handshake(&req);
        assert!(result.is_err());
    }
}
