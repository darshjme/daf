//! # DDAL Tokio Codec
//!
//! Implements [`tokio_util::codec::Encoder`] and [`Decoder`] for [`Frame`],
//! bridging the raw TCP byte stream to the typed frame world.
//!
//! The codec is stateless between frames — every frame is self-describing
//! thanks to the magic-byte prefix and length field. This means a single
//! codec instance can be shared across the lifetime of a connection without
//! worrying about sequencing state.
//!
//! ## Back-pressure
//!
//! The codec enforces [`MAX_PAYLOAD_SIZE`] on decode *and* encode, preventing
//! a misbehaving peer from forcing unbounded memory allocation. The
//! [`Framed`](tokio_util::codec::Framed) transport built on top of this codec
//! naturally provides back-pressure through Tokio's cooperative yielding.

use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use crate::protocol::{Frame, FrameDecodeError, HEADER_SIZE, MAGIC_BYTES, MAX_PAYLOAD_SIZE};

// ---------------------------------------------------------------------------
// DdalCodec
// ---------------------------------------------------------------------------

/// Tokio codec that encodes [`Frame`] values into bytes and decodes bytes
/// back into [`Frame`] values.
///
/// # Usage
///
/// ```rust,ignore
/// use tokio::net::TcpStream;
/// use tokio_util::codec::Framed;
/// use daf_ddal::codec::DdalCodec;
///
/// let stream = TcpStream::connect("127.0.0.1:9090").await?;
/// let mut transport = Framed::new(stream, DdalCodec::new());
/// ```
#[derive(Debug, Clone)]
pub struct DdalCodec {
    /// Maximum frame size (header + payload) the codec will accept.
    /// Defaults to `HEADER_SIZE + MAX_PAYLOAD_SIZE`.
    max_frame_size: usize,
}

impl DdalCodec {
    /// Create a codec with the default maximum frame size.
    pub fn new() -> Self {
        Self {
            max_frame_size: HEADER_SIZE + MAX_PAYLOAD_SIZE as usize,
        }
    }

    /// Create a codec with a custom maximum frame size.
    ///
    /// This is useful in tests or constrained environments where you want to
    /// reject frames earlier than the protocol-level maximum.
    pub fn with_max_frame_size(max_frame_size: usize) -> Self {
        Self { max_frame_size }
    }

    /// Return the configured maximum frame size.
    pub fn max_frame_size(&self) -> usize {
        self.max_frame_size
    }

    /// Scan the buffer for the next magic-byte sequence, discarding any
    /// garbage bytes before it.
    ///
    /// Returns `true` if magic bytes were found (buffer is positioned at them).
    /// Returns `false` if no magic bytes exist in the buffer.
    fn align_to_magic(buf: &mut BytesMut) -> bool {
        loop {
            if buf.len() < 4 {
                return false;
            }
            if buf[0..4] == MAGIC_BYTES {
                return true;
            }
            // Discard one byte and try again. In a healthy connection this
            // path is never taken; it only fires after stream corruption.
            buf.advance(1);
        }
    }
}

impl Default for DdalCodec {
    fn default() -> Self {
        Self::new()
    }
}

// We need this import for BytesMut::advance in align_to_magic.
use bytes::Buf;

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

impl Decoder for DdalCodec {
    type Item = Frame;
    type Error = DdalCodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Step 1: Synchronise to magic bytes. If the buffer doesn't start with
        // them, scan forward — this handles the (rare) case of mid-stream
        // corruption or partial writes from a previous connection.
        if !Self::align_to_magic(src) {
            return Ok(None); // not enough data even for magic
        }

        // Step 2: Check if we have enough for the header.
        if src.len() < HEADER_SIZE {
            // Reserve space to reduce reallocations on the next read.
            src.reserve(HEADER_SIZE - src.len());
            return Ok(None);
        }

        Frame::validate_header(src)?;

        // Step 3: Peek at the payload length (offset 12..16) to determine
        // total frame size.
        let payload_len = u32::from_be_bytes([src[12], src[13], src[14], src[15]]);
        let total = HEADER_SIZE + payload_len as usize;

        if total > self.max_frame_size {
            return Err(DdalCodecError::FrameTooLarge {
                size: total,
                max: self.max_frame_size,
            });
        }

        // Step 4: Do we have the full frame?
        if src.len() < total {
            // Let the transport grow its read buffer as bytes arrive. A peer
            // advertising a large frame and then stalling must not reserve
            // that entire payload immediately for every connection.
            return Ok(None);
        }

        // Step 5: Delegate to Frame::decode_from_bytes which handles the
        // actual parsing and checksum verification.
        match Frame::decode_from_bytes(src) {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => Ok(None),
            Err(e) => Err(DdalCodecError::Protocol(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

impl Encoder<Frame> for DdalCodec {
    type Error = DdalCodecError;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        if !crate::protocol::ProtocolVersion::CURRENT.is_compatible_with(&frame.version) {
            return Err(FrameDecodeError::UnsupportedVersion(frame.version).into());
        }
        // Frame fields are public; never trust a forged length or stale checksum.
        let total = HEADER_SIZE + frame.payload.len();
        if frame.payload.len() > MAX_PAYLOAD_SIZE as usize || total > self.max_frame_size {
            return Err(DdalCodecError::FrameTooLarge {
                size: total,
                max: self
                    .max_frame_size
                    .min(HEADER_SIZE + MAX_PAYLOAD_SIZE as usize),
            });
        }
        if frame.payload.len() != frame.payload_len as usize {
            return Err(DdalCodecError::InvalidFrame(
                "payload length does not match payload",
            ));
        }
        if !frame.verify_checksum() {
            return Err(DdalCodecError::InvalidFrame(
                "checksum does not match frame",
            ));
        }
        let encoded = frame.encode_to_bytes();
        dst.extend_from_slice(&encoded);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by [`DdalCodec`] during encoding or decoding.
#[derive(Debug, thiserror::Error)]
pub enum DdalCodecError {
    #[error("invalid frame: {0}")]
    InvalidFrame(&'static str),
    #[error("frame too large: {size} bytes exceeds codec maximum of {max}")]
    FrameTooLarge { size: usize, max: usize },

    #[error("protocol error: {0}")]
    Protocol(#[from] FrameDecodeError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FrameFlags, FrameType};
    use bytes::Bytes;

    fn codec() -> DdalCodec {
        DdalCodec::new()
    }

    #[test]
    fn forged_length_cannot_bypass_encode_limit() {
        let mut frame = Frame::data(1, Bytes::from(vec![0; 100]));
        frame.payload_len = 0;
        let mut dst = BytesMut::new();
        assert!(
            DdalCodec::with_max_frame_size(HEADER_SIZE + 10)
                .encode(frame, &mut dst)
                .is_err()
        );
        assert!(dst.is_empty());
    }

    #[test]
    fn stale_checksum_cannot_be_encoded() {
        let mut frame = Frame::data(1, Bytes::from_static(b"one"));
        frame.payload = Bytes::from_static(b"two");
        assert!(
            DdalCodec::new()
                .encode(frame, &mut BytesMut::new())
                .is_err()
        );
    }

    #[test]
    fn unsupported_outgoing_version_leaves_destination_unchanged() {
        let mut frame = Frame::control(FrameType::Ping, 0);
        frame.version.major = 99;
        frame.checksum = frame.compute_checksum();
        let mut dst = BytesMut::from(&b"existing bytes"[..]);
        let original = dst.clone();
        assert!(matches!(
            codec().encode(frame, &mut dst),
            Err(DdalCodecError::Protocol(
                FrameDecodeError::UnsupportedVersion(_)
            ))
        ));
        assert_eq!(dst, original);
    }

    #[test]
    fn invalid_type_is_rejected_without_buffering_advertised_payload() {
        let frame = Frame::control(FrameType::Ping, 0);
        let mut header = frame.encode_to_bytes();
        header[7] = 0xff;
        header[12..16].copy_from_slice(&MAX_PAYLOAD_SIZE.to_be_bytes());
        let original = header.clone();
        assert!(matches!(
            codec().decode(&mut header),
            Err(DdalCodecError::Protocol(
                FrameDecodeError::UnknownFrameType(0xff)
            ))
        ));
        assert_eq!(header, original);
        assert!(header.capacity() < MAX_PAYLOAD_SIZE as usize);
    }

    #[test]
    fn stalled_large_frame_does_not_preallocate_advertised_payload() {
        let frame = Frame::control(FrameType::Data, 1);
        let mut header = frame.encode_to_bytes();
        header[12..16].copy_from_slice(&MAX_PAYLOAD_SIZE.to_be_bytes());
        assert!(codec().decode(&mut header).unwrap().is_none());
        assert_eq!(header.len(), HEADER_SIZE);
        assert!(header.capacity() < MAX_PAYLOAD_SIZE as usize);
    }

    #[test]
    fn every_header_and_payload_split_preserves_incremental_frame() {
        let frame = Frame::data(7, Bytes::from_static(b"incremental payload"));
        let encoded = frame.encode_to_bytes();
        for split in 0..encoded.len() {
            let mut c = codec();
            let mut buf = BytesMut::from(&encoded[..split]);
            assert!(c.decode(&mut buf).unwrap().is_none(), "split {split}");
            assert_eq!(&buf[..], &encoded[..split], "split {split}");
            buf.extend_from_slice(&encoded[split..]);
            assert_eq!(c.decode(&mut buf).unwrap(), Some(frame.clone()));
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn encode_then_decode() {
        let mut c = codec();
        let frame = Frame::data(1, Bytes::from_static(b"codec test"));

        let mut buf = BytesMut::new();
        c.encode(frame.clone(), &mut buf).unwrap();

        let decoded = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.stream_id, 1);
        assert_eq!(decoded.payload, Bytes::from_static(b"codec test"));
    }

    #[test]
    fn partial_header_returns_none() {
        let mut c = codec();
        let frame = Frame::control(FrameType::Ping, 0);
        let encoded = frame.encode_to_bytes();

        // Only give it 10 bytes of the 21-byte header.
        let mut buf = BytesMut::from(&encoded[..10]);
        assert!(c.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn partial_payload_returns_none() {
        let mut c = codec();
        let frame = Frame::data(5, Bytes::from(vec![0xAB; 100]));
        let encoded = frame.encode_to_bytes();

        // Give the full header but only part of the payload.
        let mut buf = BytesMut::from(&encoded[..HEADER_SIZE + 50]);
        assert!(c.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn multiple_frames_in_buffer() {
        let mut c = codec();
        let f1 = Frame::data(1, Bytes::from_static(b"first"));
        let f2 = Frame::data(2, Bytes::from_static(b"second"));

        let mut buf = BytesMut::new();
        c.encode(f1, &mut buf).unwrap();
        c.encode(f2, &mut buf).unwrap();

        let d1 = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(d1.stream_id, 1);

        let d2 = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(d2.stream_id, 2);

        // Buffer should be drained.
        assert!(buf.is_empty());
    }

    #[test]
    fn rejects_oversized_frame_on_encode() {
        let small_codec = DdalCodec::with_max_frame_size(HEADER_SIZE + 10);
        let mut c = small_codec;
        let frame = Frame::data(1, Bytes::from(vec![0u8; 100]));

        let mut buf = BytesMut::new();
        let err = c.encode(frame, &mut buf).unwrap_err();
        assert!(matches!(err, DdalCodecError::FrameTooLarge { .. }));
    }

    #[test]
    fn rejects_oversized_frame_on_decode() {
        // Encode with a permissive codec, then decode with a strict one.
        let mut encoder = DdalCodec::new();
        let frame = Frame::data(1, Bytes::from(vec![0u8; 200]));

        let mut buf = BytesMut::new();
        encoder.encode(frame, &mut buf).unwrap();

        let mut strict = DdalCodec::with_max_frame_size(HEADER_SIZE + 100);
        let err = strict.decode(&mut buf).unwrap_err();
        assert!(matches!(err, DdalCodecError::FrameTooLarge { .. }));
    }

    #[test]
    fn garbage_before_magic_is_skipped() {
        let mut c = codec();
        let frame = Frame::control(FrameType::Pong, 42);
        let encoded = frame.encode_to_bytes();

        // Prepend garbage bytes.
        let mut buf = BytesMut::from(&[0xFF, 0xFE, 0xFD][..]);
        buf.extend_from_slice(&encoded);

        let decoded = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.frame_type, FrameType::Pong);
        assert_eq!(decoded.stream_id, 42);
    }

    #[test]
    fn flags_survive_round_trip() {
        let mut c = codec();
        let frame = Frame::new(
            FrameType::Data,
            10,
            FrameFlags::COMPRESSED | FrameFlags::PRIORITY,
            Bytes::from_static(b"flagged"),
        );

        let mut buf = BytesMut::new();
        c.encode(frame.clone(), &mut buf).unwrap();

        let decoded = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.flags, FrameFlags::COMPRESSED | FrameFlags::PRIORITY);
    }
}
