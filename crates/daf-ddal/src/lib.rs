//! # daf-ddal — Darshj's Distributed Agent Language
//!
//! Binary socket-based agent communication protocol for the DAF framework.
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`protocol`] | Wire frame format: magic bytes, checksums, frame types |
//! | [`codec`] | Tokio codec bridging TCP bytes to typed frames |
//! | [`channel`] | Multiplexed logical channels with back-pressure |
//! | [`router`] | Message routing, topic pub/sub, load balancing |
//! | [`conversation`] | Multi-turn conversation tracking and forking |
//! | [`handshake`] | Connection negotiation and authentication |
//! | [`serialization`] | Multi-format payload serialization (bincode, msgpack, json) |

pub mod channel;
pub mod codec;
pub mod conversation;
pub mod handshake;
pub mod protocol;
pub mod router;
pub mod serialization;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use channel::{Channel, ChannelError, ChannelInboundWriter, ChannelPool, ChannelState};
pub use codec::{DdalCodec, DdalCodecError};
pub use conversation::{Conversation, ConversationError, ConversationId, ConversationState, Turn};
pub use handshake::{
    complete_handshake_server, perform_handshake_client, perform_handshake_server,
    HandshakeRequest, HandshakeResponse,
};
pub use protocol::{Frame, FrameDecodeError, FrameFlags, FrameType, ProtocolVersion};
pub use router::{
    BalancingStrategy, RouteEntry, Router, RoutingTable, TopicRegistry,
};
pub use serialization::{
    deserialize_payload, detect_format, serialize_payload, PayloadFormat,
};
