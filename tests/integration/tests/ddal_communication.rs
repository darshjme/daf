//! DDAL communication integration tests.
//!
//! Tests inter-agent messaging via DDAL protocol, including conversation
//! tracking, channel multiplexing, message routing, and reconnection.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use uuid::Uuid;

use daf_core::agent::{Agent, AgentContext, AgentId, AgentKind};
use daf_core::message::{Envelope, Message, MessageKind, Priority};
use daf_integration_tests::*;

// ---------------------------------------------------------------------------
// Two agents communicating via DDAL protocol over TCP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_agents_exchange_request_response() {
    init_tracing();

    // Arrange: two agents with a bidirectional channel.
    let agent_a = Arc::new(TestAgent::specialist("code-gen"));
    let agent_b = Arc::new(TestAgent::specialist("code-review"));

    let (chan_a, chan_b) = TestChannel::pair(32);

    let id_a = AgentId::new();
    let id_b = AgentId::new();

    // Act: Agent A sends a request to Agent B.
    let request = request_message(id_a, id_b, "review this code");
    chan_a.send(request.clone()).await.expect("send request");

    // Agent B receives and processes the request.
    let received = chan_b
        .recv(Duration::from_secs(2))
        .await
        .expect("recv request");

    let ctx_b = test_context();
    agent_b
        .handle_message(&ctx_b, received.clone())
        .await
        .expect("handle message");

    // Agent B sends a response back.
    let response = Message::builder(MessageKind::Response, id_b)
        .target(id_a)
        .correlation_id(received.id)
        .payload(Bytes::from("looks good"))
        .build();
    chan_b.send(response).await.expect("send response");

    // Agent A receives the response.
    let reply = chan_a
        .recv(Duration::from_secs(2))
        .await
        .expect("recv response");

    let ctx_a = test_context();
    agent_a
        .handle_message(&ctx_a, reply.clone())
        .await
        .expect("handle reply");

    // Assert
    assert_eq!(reply.kind, MessageKind::Response);
    assert_eq!(reply.correlation_id, Some(request.id));
    assert_eq!(reply.payload_str().unwrap(), "looks good");
    assert_eq!(agent_b.message_count(), 1);
    assert_eq!(agent_a.message_count(), 1);
}

// ---------------------------------------------------------------------------
// Conversation tracking across multiple turns
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversation_tracking_across_turns() {
    init_tracing();

    let (chan_a, chan_b) = TestChannel::pair(64);

    let id_a = AgentId::new();
    let id_b = AgentId::new();

    let mut conversation_ids = Vec::new();

    // Multi-turn conversation: 5 request/response pairs.
    for turn in 0..5 {
        let payload = format!("turn-{turn}");
        let request = request_message(id_a, id_b, &payload);
        let req_id = request.id;
        conversation_ids.push(req_id);

        chan_a.send(request).await.expect("send");
        let received = chan_b.recv(Duration::from_secs(2)).await.expect("recv");

        assert_eq!(received.payload_str().unwrap(), payload);

        // Respond with correlation.
        let response = Message::builder(MessageKind::Response, id_b)
            .target(id_a)
            .correlation_id(req_id)
            .payload(Bytes::from(format!("ack-{turn}")))
            .build();
        chan_b.send(response).await.expect("send response");

        let reply = chan_a.recv(Duration::from_secs(2)).await.expect("recv reply");
        assert_eq!(reply.correlation_id, Some(req_id));
    }

    // Assert: all conversation IDs are unique.
    let unique: std::collections::HashSet<_> = conversation_ids.iter().collect();
    assert_eq!(unique.len(), 5, "each turn should have a unique message ID");
}

// ---------------------------------------------------------------------------
// Channel multiplexing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channel_multiplexing_multiple_streams() {
    init_tracing();

    let agent_count = 4;
    let hub_id = AgentId::new();

    // Create N channels from N agents to one hub agent.
    let mut agent_channels = Vec::new();
    let mut hub_channels = Vec::new();

    for _ in 0..agent_count {
        let (agent_side, hub_side) = TestChannel::pair(16);
        agent_channels.push(agent_side);
        hub_channels.push(hub_side);
    }

    // Each agent sends a message concurrently.
    let mut handles = Vec::new();
    for (i, chan) in agent_channels.iter().enumerate() {
        let msg = request_message(AgentId::new(), hub_id, &format!("from-agent-{i}"));
        let tx = chan.tx.clone();
        handles.push(tokio::spawn(async move {
            tx.send(msg).await.expect("mux send");
        }));
    }

    for h in handles {
        h.await.expect("join");
    }

    // Hub collects messages from all channels.
    let mut collected = Vec::new();
    for chan in &hub_channels {
        let msg = chan.recv(Duration::from_secs(2)).await.expect("hub recv");
        collected.push(msg);
    }

    // Assert: received exactly one message per channel.
    assert_eq!(collected.len(), agent_count);

    let payloads: Vec<String> = collected
        .iter()
        .map(|m| m.payload_str().unwrap().to_string())
        .collect();

    for i in 0..agent_count {
        assert!(
            payloads.contains(&format!("from-agent-{i}")),
            "missing message from agent {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// Message routing with multiple agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn message_routing_to_correct_target() {
    init_tracing();

    // Arrange: three agents — router, worker-a, worker-b.
    let router_id = AgentId::new();
    let worker_a_id = AgentId::new();
    let worker_b_id = AgentId::new();

    let agent_a = Arc::new(TestAgent::worker());
    let agent_b = Arc::new(TestAgent::worker());

    let (router_to_a, a_from_router) = TestChannel::pair(16);
    let (router_to_b, b_from_router) = TestChannel::pair(16);

    // Route a message intended for worker A.
    let msg_for_a = command_message(
        router_id,
        worker_a_id,
        serde_json::json!({"task": "lint"}),
    );

    // Route a message intended for worker B.
    let msg_for_b = command_message(
        router_id,
        worker_b_id,
        serde_json::json!({"task": "test"}),
    );

    // Simulate routing decision based on target.
    let target_a = msg_for_a.target.unwrap();
    let target_b = msg_for_b.target.unwrap();

    if target_a == worker_a_id {
        router_to_a.send(msg_for_a).await.expect("route to A");
    }
    if target_b == worker_b_id {
        router_to_b.send(msg_for_b).await.expect("route to B");
    }

    // Workers receive their messages.
    let received_a = a_from_router
        .recv(Duration::from_secs(2))
        .await
        .expect("A recv");
    let received_b = b_from_router
        .recv(Duration::from_secs(2))
        .await
        .expect("B recv");

    let ctx = test_context();
    agent_a
        .handle_message(&ctx, received_a.clone())
        .await
        .unwrap();
    agent_b
        .handle_message(&ctx, received_b.clone())
        .await
        .unwrap();

    // Assert: each agent got the right task.
    let payload_a: serde_json::Value = received_a.payload_json().unwrap();
    let payload_b: serde_json::Value = received_b.payload_json().unwrap();
    assert_eq!(payload_a["task"], "lint");
    assert_eq!(payload_b["task"], "test");
    assert_eq!(agent_a.message_count(), 1);
    assert_eq!(agent_b.message_count(), 1);
}

// ---------------------------------------------------------------------------
// Envelope hop tracking and loop detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn envelope_hop_tracking_and_loop_detection() {
    init_tracing();

    let source = AgentId::new();
    let msg = request_message(source, AgentId::new(), "ping");

    // Wrap in an envelope and route through hops.
    let mut envelope = Envelope::new(msg);
    envelope.max_hops = 3;

    // Three hops should succeed.
    for _ in 0..3 {
        envelope.record_hop(AgentId::new()).expect("hop should succeed");
    }
    assert_eq!(envelope.hops, 3);
    assert_eq!(envelope.route.len(), 3);

    // Fourth hop should trigger loop detection.
    let result = envelope.record_hop(AgentId::new());
    assert!(result.is_err(), "should detect routing loop");
    assert!(envelope.is_loop_detected());
}

// ---------------------------------------------------------------------------
// Handshake and reconnection simulation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_and_reconnection_simulation() {
    init_tracing();

    let client_id = AgentId::new();
    let server_id = AgentId::new();

    // Initial connection: handshake via heartbeat exchange.
    let (client_chan, server_chan) = TestChannel::pair(16);

    // Client sends handshake heartbeat.
    let handshake = heartbeat_message(client_id);
    client_chan.send(handshake).await.expect("send handshake");

    let received = server_chan
        .recv(Duration::from_secs(2))
        .await
        .expect("recv handshake");
    assert_eq!(received.kind, MessageKind::Heartbeat);
    assert_eq!(received.source, client_id);

    // Server acknowledges with its own heartbeat.
    let ack = heartbeat_message(server_id);
    server_chan.send(ack).await.expect("send ack");

    let ack_received = client_chan
        .recv(Duration::from_secs(2))
        .await
        .expect("recv ack");
    assert_eq!(ack_received.kind, MessageKind::Heartbeat);

    // Simulate connection drop: drop old channels.
    drop(client_chan);
    drop(server_chan);

    // Reconnect: create new channels (simulating TCP reconnect).
    let (new_client, new_server) = TestChannel::pair(16);

    // Re-handshake with same agent IDs.
    let re_handshake = heartbeat_message(client_id);
    new_client.send(re_handshake).await.expect("send re-handshake");

    let re_received = new_server
        .recv(Duration::from_secs(2))
        .await
        .expect("recv re-handshake");

    // Assert: same agent reconnected.
    assert_eq!(re_received.source, client_id);
    assert_eq!(re_received.kind, MessageKind::Heartbeat);
}

// ---------------------------------------------------------------------------
// Priority-based message ordering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn priority_ordering_in_buffered_channel() {
    init_tracing();

    let source = AgentId::new();
    let target = AgentId::new();

    // Send messages with different priorities into a collection.
    let messages = vec![
        Message::builder(MessageKind::Command, source)
            .target(target)
            .priority(Priority::Background)
            .payload(Bytes::from("background"))
            .build(),
        Message::builder(MessageKind::Command, source)
            .target(target)
            .priority(Priority::Critical)
            .payload(Bytes::from("critical"))
            .build(),
        Message::builder(MessageKind::Command, source)
            .target(target)
            .priority(Priority::Normal)
            .payload(Bytes::from("normal"))
            .build(),
        Message::builder(MessageKind::Command, source)
            .target(target)
            .priority(Priority::High)
            .payload(Bytes::from("high"))
            .build(),
    ];

    // Sort by priority (lower ordinal = higher priority).
    let mut sorted = messages.clone();
    sorted.sort_by_key(|m| m.priority);

    // Assert: Critical < High < Normal < Background.
    assert_eq!(sorted[0].payload_str().unwrap(), "critical");
    assert_eq!(sorted[1].payload_str().unwrap(), "high");
    assert_eq!(sorted[2].payload_str().unwrap(), "normal");
    assert_eq!(sorted[3].payload_str().unwrap(), "background");
}
