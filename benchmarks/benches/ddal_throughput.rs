//! DDAL protocol throughput benchmarks.
//!
//! Measures serialization, codec, routing, and multiplexing performance
//! across a range of payload sizes.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use bytes::Bytes;
use daf_core::message::{Envelope, Message, MessageKind, Priority};
use daf_core::{AgentId, AgentManifest, AgentKind};

// ---------------------------------------------------------------------------
// Payload sizes under test
// ---------------------------------------------------------------------------

const PAYLOAD_SIZES: &[(usize, &str)] = &[
    (64, "64B"),
    (1_024, "1KB"),
    (64 * 1_024, "64KB"),
    (1_024 * 1_024, "1MB"),
];

fn make_payload(size: usize) -> Bytes {
    Bytes::from(vec![0xABu8; size])
}

fn make_message(payload: Bytes) -> Message {
    let src = AgentId::new();
    Message::builder(MessageKind::Request, src)
        .target(AgentId::new())
        .priority(Priority::Normal)
        .payload(payload)
        .header("content-type", "application/octet-stream")
        .build()
}

// ---------------------------------------------------------------------------
// Frame serialization / deserialization
// ---------------------------------------------------------------------------

fn bench_frame_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddal/frame_serialize");

    for &(size, label) in PAYLOAD_SIZES {
        let payload = make_payload(size);
        let msg = make_message(payload);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("serialize", label), &msg, |b, msg| {
            b.iter(|| {
                let json = serde_json::to_vec(msg).expect("serialize");
                criterion::black_box(json);
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("ddal/frame_deserialize");

    for &(size, label) in PAYLOAD_SIZES {
        let payload = make_payload(size);
        let msg = make_message(payload);
        let encoded = serde_json::to_vec(&msg).expect("serialize");

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("deserialize", label),
            &encoded,
            |b, encoded| {
                b.iter(|| {
                    let msg: Message = serde_json::from_slice(encoded).expect("deserialize");
                    criterion::black_box(msg);
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Codec encode / decode round-trip
// ---------------------------------------------------------------------------

fn bench_codec_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddal/codec_roundtrip");

    for &(size, label) in PAYLOAD_SIZES {
        let payload = make_payload(size);
        let msg = make_message(payload);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("json_roundtrip", label),
            &msg,
            |b, msg| {
                b.iter(|| {
                    let encoded = serde_json::to_vec(msg).expect("encode");
                    let decoded: Message = serde_json::from_slice(&encoded).expect("decode");
                    criterion::black_box(decoded);
                });
            },
        );

        // bincode round-trip (binary codec)
        group.bench_with_input(
            BenchmarkId::new("bincode_roundtrip", label),
            &msg,
            |b, msg| {
                b.iter(|| {
                    let encoded = bincode::serialize(msg).expect("bincode encode");
                    let decoded: Message = bincode::deserialize(&encoded).expect("bincode decode");
                    criterion::black_box(decoded);
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Message routing latency (envelope hop tracking overhead)
// ---------------------------------------------------------------------------

fn bench_message_routing(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddal/routing");

    // Measure the cost of creating and routing an envelope through N hops.
    for hop_count in [1, 5, 10, 50] {
        group.bench_with_input(
            BenchmarkId::new("envelope_hops", hop_count),
            &hop_count,
            |b, &hops| {
                let payload = make_payload(256);
                let msg = make_message(payload);

                b.iter(|| {
                    let mut env = Envelope::new(msg.clone());
                    env.max_hops = hops as u32 + 1;
                    for _ in 0..hops {
                        env.record_hop(AgentId::new()).expect("hop");
                    }
                    criterion::black_box(&env);
                });
            },
        );
    }

    // Measure routing decision: find target from a set of agents.
    for agent_count in [10, 100, 1_000] {
        let agents: Vec<AgentId> = (0..agent_count).map(|_| AgentId::new()).collect();

        group.bench_with_input(
            BenchmarkId::new("find_target", agent_count),
            &agents,
            |b, agents| {
                let target = agents[agents.len() / 2]; // middle agent
                b.iter(|| {
                    let found = agents.iter().find(|&&a| a == target);
                    criterion::black_box(found);
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Channel multiplexing overhead
// ---------------------------------------------------------------------------

fn bench_channel_multiplexing(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("ddal/channel_mux");

    // Measure tokio mpsc channel throughput (simulates DDAL channel mux)
    for &(size, label) in PAYLOAD_SIZES {
        let payload = make_payload(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("mpsc_send_recv", label),
            &payload,
            |b, payload| {
                b.iter(|| {
                    rt.block_on(async {
                        let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(64);
                        let p = payload.clone();
                        tx.send(p).await.expect("send");
                        let received = rx.recv().await.expect("recv");
                        criterion::black_box(received);
                    });
                });
            },
        );
    }

    // Multi-channel fan-out: one sender, N receiver channels
    for channel_count in [2, 8, 32] {
        let payload = make_payload(256);

        group.bench_with_input(
            BenchmarkId::new("fanout_channels", channel_count),
            &(channel_count, payload.clone()),
            |b, (n, payload)| {
                b.iter(|| {
                    rt.block_on(async {
                        let mut senders = Vec::with_capacity(*n);
                        let mut receivers = Vec::with_capacity(*n);
                        for _ in 0..*n {
                            let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(16);
                            senders.push(tx);
                            receivers.push(rx);
                        }
                        // Fan out the payload to all channels
                        for tx in &senders {
                            tx.send(payload.clone()).await.expect("fanout send");
                        }
                        // Drain all
                        for rx in &mut receivers {
                            let _ = rx.recv().await.expect("fanout recv");
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Message construction throughput
// ---------------------------------------------------------------------------

fn bench_message_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddal/message_construction");

    group.bench_function("builder_minimal", |b| {
        b.iter(|| {
            let msg = Message::builder(MessageKind::Event, AgentId::new()).build();
            criterion::black_box(msg);
        });
    });

    group.bench_function("builder_full", |b| {
        b.iter(|| {
            let msg = Message::builder(MessageKind::Request, AgentId::new())
                .target(AgentId::new())
                .priority(Priority::High)
                .payload(Bytes::from_static(b"benchmark payload"))
                .header("x-trace", "bench-001")
                .header("content-type", "text/plain")
                .ttl(std::time::Duration::from_secs(30))
                .build();
            criterion::black_box(msg);
        });
    });

    group.bench_function("manifest_creation", |b| {
        b.iter(|| {
            let m = AgentManifest::new(AgentKind::Worker, "bench-worker")
                .with_capability(
                    daf_core::AgentCapability::new("compute", "1.0.0", "Computation"),
                )
                .with_metadata("team", "platform")
                .with_metadata("region", "us-east-1");
            criterion::black_box(m);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_frame_serialization,
    bench_codec_roundtrip,
    bench_message_routing,
    bench_channel_multiplexing,
    bench_message_construction,
);
criterion_main!(benches);
