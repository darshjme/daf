# DDAL Protocol Specification

**Darshj's Distributed Agent Language** — binary wire protocol for inter-agent communication in DAF.

Version: `1.0.0`  
Schema format: FlatBuffers  
File identifier: `DDAL`

---

## Building

### Prerequisites

Install the FlatBuffers compiler:

```bash
# macOS
brew install flatbuffers

# Linux (Debian/Ubuntu)
apt install flatbuffers-compiler

# From source via Cargo
cargo install flatbuffers
```

### Generate Rust bindings

```bash
./proto/build.sh
```

Output lands in `crates/daf-ddal/src/generated/`.

### Validate schema only

```bash
./proto/build.sh --check
```

---

## Wire Format

Every message on the transport is a **length-prefixed Frame**. The 4-byte
length prefix is little-endian and does NOT include itself.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                    Payload Length (LE u32)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
|               FlatBuffer-encoded Frame                        |
|                                                               |
|  +----------------------------------------------------------+ |
|  | frame_type (1B)  | flags (1B)  | stream_id (4B)          | |
|  +----------------------------------------------------------+ |
|  | sequence (8B)    | timestamp_us (8B)                     | |
|  +----------------------------------------------------------+ |
|  | payload_format   | compression | payload ([ubyte])       | |
|  +----------------------------------------------------------+ |
|  | headers          | ack_sequence | nack_reason | ...       | |
|  +----------------------------------------------------------+ |
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### Frame flags bitfield

| Bit | Mask   | Meaning                                  |
|-----|--------|------------------------------------------|
| 0   | `0x01` | Payload is compressed (see `compression`) |
| 1   | `0x02` | Payload is encrypted                     |
| 2   | `0x04` | Final fragment of a multi-frame message  |

### File magic

FlatBuffer files use the identifier `DDAL` (bytes `44 44 41 4C`).

---

## Message Flow

### Connection establishment (handshake)

```
  Agent A                                      Agent B
    |                                              |
    |--- Frame(Handshake) ----------------------->|
    |    payload = HandshakeRequest {              |
    |      protocol_version: "1.0.0",             |
    |      agent_id: "agent-a-uuid",              |
    |      supported_formats: [Bincode, Json],    |
    |      capabilities: [...]                    |
    |    }                                        |
    |                                              |
    |<-- Frame(Handshake) ------------------------|
    |    payload = HandshakeResponse {             |
    |      accepted: true,                        |
    |      protocol_version: "1.0.0",             |
    |      agent_id: "agent-b-uuid",              |
    |      chosen_format: Bincode,                |
    |      session_token: "tok_..."               |
    |    }                                        |
    |                                              |
    |    === session established ===               |
```

### Task delegation

```
  Orchestrator                                Worker
    |                                            |
    |--- Frame(Data) -------------------------->|
    |    Envelope {                              |
    |      source: "orchestrator",              |
    |      destination: "worker-7",             |
    |      payload: TaskSpec {                  |
    |        task_id: "task-001",               |
    |        instructions: "Summarize doc...",  |
    |        timeout_ms: 30000                  |
    |      }                                    |
    |    }                                      |
    |                                            |
    |<-- Frame(Ack, ack_sequence=N) ------------|
    |                                            |
    |        ... worker processes ...            |
    |                                            |
    |<-- Frame(Data) ---------------------------|
    |    Envelope {                              |
    |      source: "worker-7",                  |
    |      destination: "orchestrator",         |
    |      correlation_id: "task-001",          |
    |      payload: TaskResult {                |
    |        task_id: "task-001",               |
    |        status: Completed,                 |
    |        duration_ms: 4200                  |
    |      }                                    |
    |    }                                      |
    |                                            |
    |--- Frame(Ack) --------------------------->|
```

### Pub/Sub broadcast

```
  Publisher                   Router               Subscriber A, B
    |                           |                       |
    |--- Frame(Data) --------->|                       |
    |    Envelope {             |                       |
    |      topic: "events.*",  |                       |
    |      payload: Event {...}|                       |
    |    }                     |--- Frame(Data) ------>| (A)
    |                          |--- Frame(Data) ------>| (B)
    |                          |                       |
```

### Health check

```
  Monitor                                    Agent
    |                                          |
    |--- Frame(Ping) ----------------------->|
    |    payload = HealthCheckRequest {       |
    |      detailed: true                    |
    |    }                                   |
    |                                          |
    |<-- Frame(Pong) ------------------------|
    |    payload = HealthCheckResponse {      |
    |      healthy: true,                    |
    |      state: Ready,                     |
    |      uptime_secs: 86400,               |
    |      active_tasks: 3,                  |
    |      components: [...]                 |
    |    }                                   |
```

---

## Version Negotiation

1. The **client** sends a `HandshakeRequest` with its highest supported
   `protocol_version` (semver string, e.g. `"1.2.0"`).

2. The **server** compares the requested version against its own supported
   range and replies with the highest mutually-supported version in
   `HandshakeResponse.protocol_version`.

3. Rules:
   - Major version **must** match. A v2 client cannot speak to a v1 server.
   - The server picks `min(client_version, server_max_version)` within the
     same major.
   - If no overlap exists, the server sets `accepted = false` and fills
     `reject_reason` with a human-readable explanation.

4. After a successful handshake, both sides use the negotiated version's
   semantics for all subsequent frames on that connection.

5. Payload format and compression are negotiated the same way:
   - Client advertises `supported_formats` / `supported_compression`.
   - Server picks one of each and records it in `chosen_format` /
     `chosen_compression`.
   - All subsequent Data frames on the session use the negotiated choices.

---

## Key Tables Reference

| Table                 | Purpose                                         |
|-----------------------|-------------------------------------------------|
| `Frame`               | Wire-level transport unit                       |
| `HandshakeRequest`    | Connection initiation with version + caps       |
| `HandshakeResponse`   | Server acceptance with negotiated params        |
| `Envelope`            | Routing wrapper with source/dest/topic/TTL      |
| `Message`             | Application-level multi-modal message           |
| `ConversationTurn`    | Single turn in a conversation history           |
| `Conversation`        | Ordered turn sequence with shared context       |
| `AgentManifest`       | Agent identity, capabilities, resource limits   |
| `TaskSpec`            | Work unit specification with deps and deadlines |
| `TaskResult`          | Execution result with status and timing         |
| `Event`               | Structured event with severity and tracing      |
| `HealthCheckRequest`  | Health probe (simple or detailed)               |
| `HealthCheckResponse` | Health status with optional component breakdown |

---

## Design Decisions

- **FlatBuffers over Protobuf**: Zero-copy deserialization matters when agents
  are routing thousands of frames per second. FlatBuffers lets us read fields
  directly from the wire buffer without allocating.

- **Length-prefixed framing**: Simple and battle-tested. The 4-byte LE prefix
  gives us up to 4 GiB frames which is more than sufficient.

- **Pluggable payload format**: The `PayloadFormat` enum lets agents negotiate
  the encoding that fits their constraints. High-throughput Rust agents use
  Bincode; debug tooling uses JSON.

- **Envelope TTL and hop counting**: Prevents routing loops in mesh topologies.
  Default TTL of 16 hops is generous for most deployments.

- **UUIDv7 message ids**: Time-ordered UUIDs give natural chronological
  ordering without a central sequence authority.
