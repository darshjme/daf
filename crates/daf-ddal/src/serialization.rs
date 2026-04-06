//! # Payload Serialization
//!
//! Multi-format serialization layer for DDAL payloads. Agents can choose
//! between [`Bincode`](PayloadFormat::Bincode) (fast, compact, Rust-native),
//! [`MessagePack`](PayloadFormat::MessagePack) (cross-language compact
//! binary), [`Json`](PayloadFormat::Json) (human-readable, debuggable),
//! or [`Raw`](PayloadFormat::Raw) (pass-through bytes).
//!
//! ## Format detection
//!
//! [`detect_format`] uses heuristic inspection of the first byte to guess
//! which format was used. This is best-effort — when ambiguous, it
//! defaults to [`Bincode`](PayloadFormat::Bincode).

use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use daf_core::{DafError, DafResult};

// ---------------------------------------------------------------------------
// PayloadFormat
// ---------------------------------------------------------------------------

/// Supported serialization formats for DDAL payloads.
///
/// The format is negotiated during the handshake or specified per-message
/// in the frame metadata. All formats produce and consume `&[u8]` so they
/// can ride on the same [`Frame`](crate::protocol::Frame) payload field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadFormat {
    /// Bincode — Rust-native binary format. Very fast, very compact, but
    /// not portable across languages without a schema.
    Bincode,
    /// MessagePack — cross-language binary format. Slightly larger than
    /// bincode but readable from Python, Go, JS, etc.
    MessagePack,
    /// JSON — human-readable text format. Largest wire size but invaluable
    /// for debugging and interop with HTTP APIs.
    Json,
    /// Raw bytes — no serialization. The payload is passed through as-is.
    /// Useful for pre-serialized data or opaque binary blobs.
    Raw,
}

impl PayloadFormat {
    /// Return a human-readable name for this format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bincode => "bincode",
            Self::MessagePack => "msgpack",
            Self::Json => "json",
            Self::Raw => "raw",
        }
    }
}

impl std::fmt::Display for PayloadFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Serialize a value into bytes using the specified format.
///
/// # Errors
///
/// Returns [`DafError::SerializationError`] if the value cannot be
/// serialized in the chosen format.
pub fn serialize_payload<T: Serialize>(value: &T, format: PayloadFormat) -> DafResult<Bytes> {
    match format {
        PayloadFormat::Bincode => {
            let data = bincode::serialize(value)
                .map_err(|e| DafError::SerializationError(format!("bincode: {e}")))?;
            Ok(Bytes::from(data))
        }
        PayloadFormat::MessagePack => {
            let data = rmp_serde::to_vec(value)
                .map_err(|e| DafError::SerializationError(format!("msgpack: {e}")))?;
            Ok(Bytes::from(data))
        }
        PayloadFormat::Json => {
            let data = serde_json::to_vec(value)
                .map_err(|e| DafError::SerializationError(format!("json: {e}")))?;
            Ok(Bytes::from(data))
        }
        PayloadFormat::Raw => {
            // For raw format, we serialize as bincode since we need some
            // serialization — the caller should use Bytes directly if they
            // truly want raw pass-through.
            let data = bincode::serialize(value)
                .map_err(|e| DafError::SerializationError(format!("raw/bincode: {e}")))?;
            Ok(Bytes::from(data))
        }
    }
}

// ---------------------------------------------------------------------------
// Deserialization
// ---------------------------------------------------------------------------

/// Deserialize a value from bytes using the specified format.
///
/// # Errors
///
/// Returns [`DafError::SerializationError`] if the data cannot be
/// deserialized in the chosen format.
pub fn deserialize_payload<T: DeserializeOwned>(
    data: &[u8],
    format: PayloadFormat,
) -> DafResult<T> {
    match format {
        PayloadFormat::Bincode => bincode::deserialize(data)
            .map_err(|e| DafError::SerializationError(format!("bincode: {e}"))),
        PayloadFormat::MessagePack => rmp_serde::from_slice(data)
            .map_err(|e| DafError::SerializationError(format!("msgpack: {e}"))),
        PayloadFormat::Json => serde_json::from_slice(data)
            .map_err(|e| DafError::SerializationError(format!("json: {e}"))),
        PayloadFormat::Raw => bincode::deserialize(data)
            .map_err(|e| DafError::SerializationError(format!("raw/bincode: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

/// Heuristically detect the serialization format of a byte slice.
///
/// Inspects the first byte to make a best-effort guess:
///
/// - `{` (0x7B) or `[` (0x5B) — likely JSON
/// - `0x80..=0xDF` — MessagePack map/array/fixstr range
/// - Everything else — assumed to be Bincode
///
/// Returns [`PayloadFormat::Raw`] for empty input.
///
/// This is a heuristic and can produce false positives. When the format
/// is known in advance (e.g. from handshake negotiation), prefer passing
/// the format explicitly rather than relying on detection.
pub fn detect_format(data: &[u8]) -> PayloadFormat {
    if data.is_empty() {
        return PayloadFormat::Raw;
    }

    match data[0] {
        // JSON objects and arrays
        b'{' | b'[' => PayloadFormat::Json,
        // MessagePack:
        //   0x80..=0x8F — fixmap
        //   0x90..=0x9F — fixarray
        //   0xA0..=0xBF — fixstr
        //   0xC0..=0xDF — various msgpack type markers (nil, bool, bin, ext, float, etc.)
        0x80..=0xDF => PayloadFormat::MessagePack,
        // Default assumption for everything else.
        _ => PayloadFormat::Bincode,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestPayload {
        name: String,
        value: u64,
        tags: Vec<String>,
    }

    fn sample_payload() -> TestPayload {
        TestPayload {
            name: "test".into(),
            value: 42,
            tags: vec!["alpha".into(), "beta".into()],
        }
    }

    #[test]
    fn bincode_round_trip() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::Bincode).unwrap();
        let back: TestPayload = deserialize_payload(&bytes, PayloadFormat::Bincode).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn msgpack_round_trip() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::MessagePack).unwrap();
        let back: TestPayload =
            deserialize_payload(&bytes, PayloadFormat::MessagePack).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn json_round_trip() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::Json).unwrap();
        // JSON should be human-readable.
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"name\""));
        assert!(text.contains("\"test\""));

        let back: TestPayload = deserialize_payload(&bytes, PayloadFormat::Json).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn raw_round_trip() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::Raw).unwrap();
        let back: TestPayload = deserialize_payload(&bytes, PayloadFormat::Raw).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn detect_json_object() {
        let data = br#"{"key": "value"}"#;
        assert_eq!(detect_format(data), PayloadFormat::Json);
    }

    #[test]
    fn detect_json_array() {
        let data = br#"[1, 2, 3]"#;
        assert_eq!(detect_format(data), PayloadFormat::Json);
    }

    #[test]
    fn detect_msgpack() {
        // 0x82 is a fixmap with 2 entries in MessagePack.
        let data = &[0x82, 0xA4, 0x6E, 0x61, 0x6D, 0x65];
        assert_eq!(detect_format(data), PayloadFormat::MessagePack);
    }

    #[test]
    fn detect_bincode_fallback() {
        // Bincode typically starts with a length prefix (little-endian u64).
        let data = &[0x04, 0x00, 0x00, 0x00];
        assert_eq!(detect_format(data), PayloadFormat::Bincode);
    }

    #[test]
    fn detect_empty_is_raw() {
        assert_eq!(detect_format(&[]), PayloadFormat::Raw);
    }

    #[test]
    fn detect_matches_actual_json_serialization() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::Json).unwrap();
        assert_eq!(detect_format(&bytes), PayloadFormat::Json);
    }

    #[test]
    fn detect_matches_actual_msgpack_serialization() {
        let payload = sample_payload();
        let bytes = serialize_payload(&payload, PayloadFormat::MessagePack).unwrap();
        assert_eq!(detect_format(&bytes), PayloadFormat::MessagePack);
    }

    #[test]
    fn msgpack_is_smaller_than_json() {
        let payload = sample_payload();
        let json = serialize_payload(&payload, PayloadFormat::Json).unwrap();
        let msgpack = serialize_payload(&payload, PayloadFormat::MessagePack).unwrap();
        assert!(
            msgpack.len() < json.len(),
            "msgpack ({}) should be smaller than json ({})",
            msgpack.len(),
            json.len(),
        );
    }

    #[test]
    fn format_display() {
        assert_eq!(PayloadFormat::Bincode.to_string(), "bincode");
        assert_eq!(PayloadFormat::MessagePack.to_string(), "msgpack");
        assert_eq!(PayloadFormat::Json.to_string(), "json");
        assert_eq!(PayloadFormat::Raw.to_string(), "raw");
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = &[0xFF, 0xFE, 0xFD];
        let result = deserialize_payload::<TestPayload>(garbage, PayloadFormat::Json);
        assert!(result.is_err());
    }

    #[test]
    fn serde_round_trip_payload_format() {
        let format = PayloadFormat::MessagePack;
        let json = serde_json::to_string(&format).unwrap();
        let back: PayloadFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, format);
    }

    #[test]
    fn serialize_primitive_types() {
        // Ensure we can serialize simple types, not just structs.
        let num: u64 = 12345;
        let bytes = serialize_payload(&num, PayloadFormat::Bincode).unwrap();
        let back: u64 = deserialize_payload(&bytes, PayloadFormat::Bincode).unwrap();
        assert_eq!(back, num);

        let s = "hello world".to_string();
        let bytes = serialize_payload(&s, PayloadFormat::Json).unwrap();
        let back: String = deserialize_payload(&bytes, PayloadFormat::Json).unwrap();
        assert_eq!(back, s);
    }
}
