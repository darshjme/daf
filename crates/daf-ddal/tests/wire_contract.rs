//! The same fixture runs in DAF and DJcode's bundled DDAL crate.

use bytes::{Bytes, BytesMut};
use daf_ddal::{DdalCodec, Frame, FrameType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};
use tokio_util::codec::{Decoder, Encoder};

// DDAL 0.1.0, Data, stream 0x01020304, seven payload bytes, no flags.
// BLAKE3(header[0..17] || "fixture") starts e7266272, independently computed.
const GOLDEN: &[u8] = &[
    0xda, 0xf0, 0xdd, 0xa1, 0x00, 0x01, 0x00, 0x02, 0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x00, 0x07,
    0x00, 0xe7, 0x26, 0x62, 0x72, b'f', b'i', b'x', b't', b'u', b'r', b'e',
];

#[test]
fn golden_data_frame_is_wire_stable() {
    let expected = Frame::data(0x01020304, Bytes::from_static(b"fixture"));
    let mut codec = DdalCodec::new();
    let mut encoded = BytesMut::new();
    codec.encode(expected.clone(), &mut encoded).unwrap();
    assert_eq!(encoded.as_ref(), GOLDEN);
    assert_eq!(codec.decode(&mut encoded).unwrap(), Some(expected));
    assert!(encoded.is_empty());
}

#[test]
fn checksum_protects_routing_flags_and_payload() {
    for offset in [8, 16, 21] {
        let mut bytes = BytesMut::from(GOLDEN);
        bytes[offset] ^= 0x01;
        assert!(DdalCodec::new().decode(&mut bytes).is_err());
    }
}

#[test]
fn truncated_payload_at_eof_is_rejected() {
    let mut bytes = BytesMut::from(&GOLDEN[..GOLDEN.len() - 1]);
    assert!(DdalCodec::new().decode(&mut bytes).unwrap().is_none());
    assert!(DdalCodec::new().decode_eof(&mut bytes).is_err());
}

#[tokio::test]
async fn malformed_tcp_header_is_rejected_while_peer_keeps_connection_open() {
    for fault in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = [0; 21];
            socket.read_exact(&mut header).await.unwrap();
            DdalCodec::new().decode(&mut BytesMut::from(header.as_slice()))
        });
        let mut peer = TcpStream::connect(addr).await.unwrap();
        let mut header = GOLDEN[..21].to_vec();
        match fault {
            0 => header[7] = 0xff,
            1 => header[4] = 0xff,
            _ => header[12..16].copy_from_slice(&u32::MAX.to_be_bytes()),
        }
        peer.write_all(&header).await.unwrap();
        // Do not send payload or EOF. Header validation must finish unaided.
        assert!(
            timeout(Duration::from_secs(1), server)
                .await
                .expect("decoder waited for a malformed header's advertised payload")
                .unwrap()
                .is_err()
        );
    }
}

#[tokio::test]
async fn fragmented_tcp_frame_decodes_before_peer_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = BytesMut::new();
        let mut codec = DdalCodec::new();
        loop {
            assert_ne!(socket.read_buf(&mut bytes).await.unwrap(), 0);
            if let Some(frame) = codec.decode(&mut bytes).unwrap() {
                return frame;
            }
        }
    });
    let mut peer = TcpStream::connect(addr).await.unwrap();
    for byte in GOLDEN {
        peer.write_all(&[*byte]).await.unwrap();
        tokio::task::yield_now().await;
    }
    let frame = timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame.frame_type, FrameType::Data);
    assert_eq!(frame.stream_id, 0x01020304);
    assert_eq!(frame.payload, Bytes::from_static(b"fixture"));
}
