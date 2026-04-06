//! # DDAL Wire Protocol
//!
//! Defines the binary framing format that every DDAL message rides on the wire.
//!
//! ## Frame Layout (network byte order, big-endian)
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!  0       4    Magic bytes  [0xDA, 0xF0, 0xDD, 0xA1]
//!  4       1    Protocol version major
//!  5       1    Protocol version minor
//!  6       1    Protocol version patch
//!  7       1    Frame type   (discriminant of `FrameType`)
//!  8       4    Stream ID    (multiplexing key)
//! 12       4    Payload length (bytes, max 16 MiB)
//! 16       1    Flags        (bitfield — see `FrameFlags`)
//! 17       4    Checksum     (truncated blake3 of header[0..17] ++ payload)
//! 21       N    Payload      (N = payload_len)
//! ```
//!
//! Total header size: **21 bytes**, deliberately small to minimise overhead on
//! the high-frequency ping/ack path while still carrying enough metadata for
//! the router and codec to do their jobs without touching the payload.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Magic bytes that open every DDAL frame.
///
/// Mnemonic: **DA**F-**0**-**DD**AL-**A1**pha.
/// Used by the codec to re-synchronise after partial reads or corrupt streams.
pub const MAGIC_BYTES: [u8; 4] = [0xDA, 0xF0, 0xDD, 0xA1];

/// Fixed size of the frame header in bytes (everything before the payload).
pub const HEADER_SIZE: usize = 21;

/// Maximum allowed payload size: 16 MiB.
///
/// Agents exchanging payloads larger than this should use the streaming
/// extension (chunked `Data` frames with the `MORE_FRAGMENTS` flag set).
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// ProtocolVersion
// ---------------------------------------------------------------------------

/// Semantic version of the DDAL protocol spoken by a peer.
///
/// Carried in every frame header so that a receiver can detect version skew
/// immediately, without waiting for the handshake to complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl ProtocolVersion {
    /// The protocol version implemented by this build of the library.
    pub const CURRENT: Self = Self {
        major: 0,
        minor: 1,
        patch: 0,
    };

    /// Returns `true` if `other` is wire-compatible with `self`.
    ///
    /// Compatibility rule: same major version, `other.minor <= self.minor`.
    /// This lets newer servers talk to older clients as long as no breaking
    /// changes were introduced.
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self.major == other.major && other.minor <= self.minor
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ---------------------------------------------------------------------------
// FrameType
// ---------------------------------------------------------------------------

/// Discriminant for the kind of frame being sent.
///
/// The numeric values are part of the wire protocol and must never be
/// reordered. New variants must be appended with the next sequential value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FrameType {
    /// Initial connection negotiation.
    Handshake = 0x01,
    /// Application data payload.
    Data = 0x02,
    /// Positive acknowledgement of a prior frame.
    Ack = 0x03,
    /// Negative acknowledgement — the peer rejected a frame.
    Nack = 0x04,
    /// Keepalive probe (expects `Pong`).
    Ping = 0x05,
    /// Keepalive response.
    Pong = 0x06,
    /// Routing-table update.
    Route = 0x07,
    /// Subscribe to a topic / agent group.
    Subscribe = 0x08,
    /// Unsubscribe from a topic / agent group.
    Unsubscribe = 0x09,
    /// Graceful connection teardown.
    Close = 0x0A,
}

impl FrameType {
    /// Decode a raw byte into a `FrameType`, returning `None` for unknown
    /// discriminants so that forward-compatible peers can skip frames they
    /// don't understand rather than crashing.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Handshake),
            0x02 => Some(Self::Data),
            0x03 => Some(Self::Ack),
            0x04 => Some(Self::Nack),
            0x05 => Some(Self::Ping),
            0x06 => Some(Self::Pong),
            0x07 => Some(Self::Route),
            0x08 => Some(Self::Subscribe),
            0x09 => Some(Self::Unsubscribe),
            0x0A => Some(Self::Close),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// FrameFlags
// ---------------------------------------------------------------------------

/// Bit-flags carried in the one-byte `flags` field of every frame.
///
/// Multiple flags can be ORed together.
pub struct FrameFlags;

impl FrameFlags {
    /// This frame is part of a multi-frame message and more fragments follow.
    pub const MORE_FRAGMENTS: u8 = 0b0000_0001;
    /// The payload is compressed (algorithm negotiated during handshake).
    pub const COMPRESSED: u8 = 0b0000_0010;
    /// The sender requests an explicit `Ack` for this frame.
    pub const ACK_REQUIRED: u8 = 0b0000_0100;
    /// This frame carries a conversation-tracking header before the payload.
    pub const HAS_CONVERSATION: u8 = 0b0000_1000;
    /// Priority frame — should jump the send queue.
    pub const PRIORITY: u8 = 0b0001_0000;
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// A single DDAL protocol frame — the atomic unit on the wire.
///
/// `Frame` is the lowest-level abstraction in the stack; everything above
/// (channels, conversations, routing) is built by composing frames.
///
/// # Checksumming
///
/// The checksum covers the header bytes `[0..17]` (everything before the
/// checksum field itself) concatenated with the payload. We use blake3 for
/// speed and truncate to 32 bits — this is a corruption detector, not a
/// cryptographic MAC. Authentication happens at the handshake layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Protocol version of the sender.
    pub version: ProtocolVersion,
    /// What kind of frame this is.
    pub frame_type: FrameType,
    /// Multiplexing stream identifier — each logical channel gets its own.
    pub stream_id: u32,
    /// Length of `payload` in bytes.
    pub payload_len: u32,
    /// Bit-flags (see [`FrameFlags`]).
    pub flags: u8,
    /// Truncated blake3 checksum for integrity verification.
    pub checksum: u32,
    /// The frame body. May be empty for control frames like Ping/Pong.
    pub payload: Bytes,
}

impl Frame {
    // -- constructors -------------------------------------------------------

    /// Build a new frame, computing the payload length and checksum
    /// automatically.
    pub fn new(frame_type: FrameType, stream_id: u32, flags: u8, payload: Bytes) -> Self {
        let payload_len = payload.len() as u32;
        let mut frame = Self {
            version: ProtocolVersion::CURRENT,
            frame_type,
            stream_id,
            payload_len,
            flags,
            checksum: 0, // placeholder
            payload,
        };
        frame.checksum = frame.compute_checksum();
        frame
    }

    /// Convenience: create an empty control frame (Ping, Pong, Ack, etc.).
    pub fn control(frame_type: FrameType, stream_id: u32) -> Self {
        Self::new(frame_type, stream_id, 0, Bytes::new())
    }

    /// Convenience: create a `Data` frame carrying `payload` on `stream_id`.
    pub fn data(stream_id: u32, payload: Bytes) -> Self {
        Self::new(FrameType::Data, stream_id, 0, payload)
    }

    // -- checksumming -------------------------------------------------------

    /// Compute the blake3 checksum for this frame.
    ///
    /// The input to the hash is: header bytes `[0..17]` (magic + version +
    /// frame_type + stream_id + payload_len + flags) concatenated with the
    /// payload. We take the first 4 bytes of the blake3 output as a `u32`.
    pub fn compute_checksum(&self) -> u32 {
        let mut hasher = blake3::Hasher::new();

        // Feed the header fields in wire order.
        hasher.update(&MAGIC_BYTES);
        hasher.update(&[self.version.major, self.version.minor, self.version.patch]);
        hasher.update(&[self.frame_type as u8]);
        hasher.update(&self.stream_id.to_be_bytes());
        hasher.update(&self.payload_len.to_be_bytes());
        hasher.update(&[self.flags]);

        // Feed the payload.
        hasher.update(&self.payload);

        let hash = hasher.finalize();
        let bytes = hash.as_bytes();
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Verify that the stored checksum matches the computed one.
    pub fn verify_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    // -- serialization ------------------------------------------------------

    /// Serialize the frame into a byte buffer suitable for writing to a socket.
    ///
    /// The caller (typically [`DdalCodec`](crate::codec::DdalCodec)) writes
    /// the returned bytes directly to the transport.
    pub fn encode_to_bytes(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(HEADER_SIZE + self.payload_len as usize);

        // Magic
        buf.put_slice(&MAGIC_BYTES);
        // Version
        buf.put_u8(self.version.major);
        buf.put_u8(self.version.minor);
        buf.put_u8(self.version.patch);
        // Frame type
        buf.put_u8(self.frame_type as u8);
        // Stream ID
        buf.put_u32(self.stream_id);
        // Payload length
        buf.put_u32(self.payload_len);
        // Flags
        buf.put_u8(self.flags);
        // Checksum
        buf.put_u32(self.checksum);
        // Payload
        buf.put_slice(&self.payload);

        buf
    }

    /// Deserialize a frame from a byte buffer.
    ///
    /// The buffer must contain at least [`HEADER_SIZE`] bytes. If the payload
    /// length field indicates more data than is available the function returns
    /// `None` (partial read — the caller should buffer more bytes and retry).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the magic bytes are wrong, the frame type is unknown,
    /// or the payload length exceeds [`MAX_PAYLOAD_SIZE`].
    pub fn decode_from_bytes(buf: &mut BytesMut) -> Result<Option<Self>, FrameDecodeError> {
        if buf.len() < HEADER_SIZE {
            return Ok(None); // need more data
        }

        // Peek at magic bytes without consuming.
        if buf[0..4] != MAGIC_BYTES {
            return Err(FrameDecodeError::InvalidMagic {
                got: [buf[0], buf[1], buf[2], buf[3]],
            });
        }

        // Peek at payload_len to know total frame size.
        let payload_len =
            u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);

        if payload_len > MAX_PAYLOAD_SIZE {
            return Err(FrameDecodeError::PayloadTooLarge {
                size: payload_len,
                max: MAX_PAYLOAD_SIZE,
            });
        }

        let total_frame_size = HEADER_SIZE + payload_len as usize;
        if buf.len() < total_frame_size {
            return Ok(None); // need more data
        }

        // Now consume the bytes.
        let mut frame_buf = buf.split_to(total_frame_size);

        // Magic (already validated)
        frame_buf.advance(4);

        // Version
        let major = frame_buf.get_u8();
        let minor = frame_buf.get_u8();
        let patch = frame_buf.get_u8();
        let version = ProtocolVersion { major, minor, patch };

        // Frame type
        let frame_type_byte = frame_buf.get_u8();
        let frame_type = FrameType::from_u8(frame_type_byte).ok_or(
            FrameDecodeError::UnknownFrameType(frame_type_byte),
        )?;

        // Stream ID
        let stream_id = frame_buf.get_u32();

        // Payload length (already peeked)
        let _payload_len = frame_buf.get_u32();

        // Flags
        let flags = frame_buf.get_u8();

        // Checksum
        let checksum = frame_buf.get_u32();

        // Payload
        let payload = frame_buf.copy_to_bytes(payload_len as usize);

        let frame = Frame {
            version,
            frame_type,
            stream_id,
            payload_len,
            flags,
            checksum,
            payload,
        };

        // Verify integrity.
        if !frame.verify_checksum() {
            return Err(FrameDecodeError::ChecksumMismatch {
                expected: frame.compute_checksum(),
                got: checksum,
            });
        }

        Ok(Some(frame))
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame({:?} stream={} len={} flags=0x{:02X})",
            self.frame_type, self.stream_id, self.payload_len, self.flags,
        )
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur while decoding a frame from the wire.
#[derive(Debug, thiserror::Error)]
pub enum FrameDecodeError {
    #[error("invalid magic bytes: expected {expected:02X?}, got {got:02X?}", expected = MAGIC_BYTES)]
    InvalidMagic { got: [u8; 4] },

    #[error("unknown frame type discriminant: 0x{0:02X}")]
    UnknownFrameType(u8),

    #[error("payload too large: {size} bytes exceeds maximum {max}")]
    PayloadTooLarge { size: u32, max: u32 },

    #[error("checksum mismatch: expected 0x{expected:08X}, got 0x{got:08X}")]
    ChecksumMismatch { expected: u32, got: u32 },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_display() {
        assert_eq!(ProtocolVersion::CURRENT.to_string(), "0.1.0");
    }

    #[test]
    fn protocol_version_compatibility() {
        let v010 = ProtocolVersion {
            major: 0,
            minor: 1,
            patch: 0,
        };
        let v020 = ProtocolVersion {
            major: 0,
            minor: 2,
            patch: 0,
        };
        let v100 = ProtocolVersion {
            major: 1,
            minor: 0,
            patch: 0,
        };

        // v0.2 is compatible with v0.1 (higher minor can talk to lower)
        assert!(v020.is_compatible_with(&v010));
        // v0.1 is NOT compatible with v0.2 (lower minor cannot understand higher)
        assert!(!v010.is_compatible_with(&v020));
        // Different major versions are never compatible.
        assert!(!v100.is_compatible_with(&v010));
    }

    #[test]
    fn frame_type_round_trip() {
        for byte in 0x01..=0x0A {
            let ft = FrameType::from_u8(byte).expect("known frame type");
            assert_eq!(ft as u8, byte);
        }
        assert!(FrameType::from_u8(0xFF).is_none());
    }

    #[test]
    fn frame_checksum_stability() {
        let frame = Frame::data(42, Bytes::from_static(b"hello, agents"));
        let checksum1 = frame.compute_checksum();
        let checksum2 = frame.compute_checksum();
        assert_eq!(checksum1, checksum2, "checksum must be deterministic");
        assert!(frame.verify_checksum());
    }

    #[test]
    fn frame_checksum_detects_corruption() {
        let mut frame = Frame::data(1, Bytes::from_static(b"important data"));
        frame.payload = Bytes::from_static(b"corrupted data");
        // Checksum was computed for "important data", now payload is different.
        assert!(!frame.verify_checksum());
    }

    #[test]
    fn frame_encode_decode_round_trip() {
        let original = Frame::new(
            FrameType::Data,
            7,
            FrameFlags::ACK_REQUIRED | FrameFlags::HAS_CONVERSATION,
            Bytes::from_static(b"round-trip payload"),
        );

        let encoded = original.encode_to_bytes();
        assert_eq!(encoded.len(), HEADER_SIZE + original.payload_len as usize);

        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Frame::decode_from_bytes(&mut buf)
            .expect("decode should succeed")
            .expect("full frame is available");

        assert_eq!(decoded.version, original.version);
        assert_eq!(decoded.frame_type, original.frame_type);
        assert_eq!(decoded.stream_id, original.stream_id);
        assert_eq!(decoded.payload_len, original.payload_len);
        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.checksum, original.checksum);
        assert_eq!(decoded.payload, original.payload);
    }

    #[test]
    fn frame_decode_partial_returns_none() {
        let frame = Frame::control(FrameType::Ping, 0);
        let encoded = frame.encode_to_bytes();

        // Feed only half the header.
        let mut buf = BytesMut::from(&encoded[..10]);
        let result = Frame::decode_from_bytes(&mut buf).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn frame_decode_bad_magic() {
        let mut buf = BytesMut::from(&[0x00, 0x00, 0x00, 0x00, 0x00][..]);
        buf.extend_from_slice(&[0u8; HEADER_SIZE]);
        let err = Frame::decode_from_bytes(&mut buf).unwrap_err();
        assert!(matches!(err, FrameDecodeError::InvalidMagic { .. }));
    }

    #[test]
    fn frame_decode_payload_too_large() {
        let mut buf = BytesMut::with_capacity(HEADER_SIZE);
        buf.put_slice(&MAGIC_BYTES);
        buf.put_u8(0); // major
        buf.put_u8(1); // minor
        buf.put_u8(0); // patch
        buf.put_u8(FrameType::Data as u8);
        buf.put_u32(0); // stream_id
        buf.put_u32(MAX_PAYLOAD_SIZE + 1); // too large
        buf.put_u8(0); // flags
        buf.put_u32(0); // checksum (won't be checked)

        let err = Frame::decode_from_bytes(&mut buf).unwrap_err();
        assert!(matches!(err, FrameDecodeError::PayloadTooLarge { .. }));
    }

    #[test]
    fn empty_control_frame_round_trip() {
        let original = Frame::control(FrameType::Pong, 999);
        let encoded = original.encode_to_bytes();
        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Frame::decode_from_bytes(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.frame_type, FrameType::Pong);
        assert_eq!(decoded.stream_id, 999);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn all_frame_types_encode() {
        let types = [
            FrameType::Handshake,
            FrameType::Data,
            FrameType::Ack,
            FrameType::Nack,
            FrameType::Ping,
            FrameType::Pong,
            FrameType::Route,
            FrameType::Subscribe,
            FrameType::Unsubscribe,
            FrameType::Close,
        ];
        for ft in types {
            let frame = Frame::control(ft, 0);
            let encoded = frame.encode_to_bytes();
            let mut buf = BytesMut::from(&encoded[..]);
            let decoded = Frame::decode_from_bytes(&mut buf).unwrap().unwrap();
            assert_eq!(decoded.frame_type, ft);
        }
    }

    #[test]
    fn header_size_matches_layout() {
        // 4 magic + 3 version + 1 type + 4 stream_id + 4 payload_len + 1 flags + 4 checksum = 21
        assert_eq!(HEADER_SIZE, 21);
    }

    #[test]
    fn frame_display() {
        let frame = Frame::data(42, Bytes::from_static(b"test"));
        let display = format!("{}", frame);
        assert!(display.contains("Data"));
        assert!(display.contains("42"));
    }
}
