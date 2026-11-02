//! Small, assertion-preserving attribution companion to the unchanged 9,999-edit test.
//! The first argument is a recorded profile label, not a compiler-setting override.

use md_crdt::core::{OpId, Sequence, SequenceOp};
use md_crdt::filesync::VaultSession;
use md_crdt::perf::{self, PerfSnapshot, Span};
use md_crdt::{EquivalenceMode, ValidationLimits};
use md_crdt_naive_oracle::Sequence as OracleSequence;
use std::time::Instant;

fn report(profile: &str, workload: &str, size: usize, wall_ns: u128, stats: &PerfSnapshot) {
    println!(
        "{}",
        serde_json::json!({
            "harness": "debug-ordering-v1",
            "profile_label": profile,
            "debug_assertions": cfg!(debug_assertions),
            "sequence_incremental": Sequence::<u8>::incremental_ordering_enabled(),
            "perf_trace": cfg!(feature = "perf_trace"),
            "workload": workload,
            "size": size,
            "wall_ns": wall_ns,
            "oracle_calls": stats.count(Span::SequenceOrderOracle),
            "oracle_inclusive_ns": stats.nanos(Span::SequenceOrderOracle),
            "sequence_apply_inclusive_ns": stats.nanos(Span::SequenceApply),
            "all_span_counts": stats.counts,
            "all_span_inclusive_ns": stats.nanos,
        })
    );
}

fn mixed_sequence(profile: &str, size: usize) {
    let mut ops = Vec::new();
    let mut prior = None;
    for index in 0..size {
        let parent = OpId {
            counter: index as u64 * 4 + 1,
            peer: 1,
        };
        let child = OpId {
            counter: index as u64 * 4 + 2,
            peer: 2,
        };
        let sibling = OpId {
            counter: index as u64 * 4 + 3,
            peer: 3,
        };
        // Missing anchors, concurrent siblings with right origins, and retained
        // tombstones all reach the same complete ordering assertion as append.
        ops.push(SequenceOp::Insert {
            after: Some(parent),
            id: child,
            value: 2u8,
            right_origin: None,
        });
        ops.push(SequenceOp::Insert {
            after: Some(parent),
            id: sibling,
            value: 3,
            right_origin: Some(child),
        });
        ops.push(SequenceOp::Delete {
            target: child,
            id: OpId {
                counter: index as u64 * 4 + 4,
                peer: 4,
            },
        });
        ops.push(SequenceOp::Insert {
            after: prior,
            id: parent,
            value: 1,
            right_origin: None,
        });
        prior = Some(parent);
    }
    let mut sequence = Sequence::new();
    let start = Instant::now();
    let (_, stats) = perf::measure(|| {
        for op in &ops {
            sequence.apply(op.clone());
        }
    });
    let wall_ns = start.elapsed().as_nanos();
    let mut oracle = OracleSequence::new();
    // The simple independent oracle discards inserts whose anchors are absent.
    // Replay the same generated history in its supported causal schedule;
    // production above still receives every missing-anchor/tombstone transition.
    let mut causal_ops = ops;
    causal_ops.sort_by_key(|operation| match operation {
        SequenceOp::Insert { id, .. } | SequenceOp::Delete { id, .. } => *id,
    });
    for operation in causal_ops {
        oracle.apply(operation);
    }
    assert_eq!(sequence.to_vec(), oracle.elements());
    assert_eq!(sequence.element_ids().len(), size * 3);
    if cfg!(feature = "perf_trace") {
        assert!(stats.count(Span::SequenceOrderOracle) > 0);
    }
    report(profile, "mixed-sequence", size, wall_ns, &stats);
}

fn vault_append_replay(profile: &str, size: usize) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    let before = vault.session_mut("note.md").unwrap().state_vector();
    let start = Instant::now();
    let (_, edit_stats) = perf::measure(|| {
        vault
            .apply_local_edit("note.md", |session| {
                for index in 0..size {
                    session.insert_text(id, 1 + index * 6, "abcdef").unwrap();
                }
            })
            .unwrap();
    });
    let edit_wall_ns = start.elapsed().as_nanos();
    let delta = vault
        .session_mut("note.md")
        .unwrap()
        .encode_changes_since(&before)
        .unwrap();
    assert_eq!(delta.ops.len(), size);
    let payload_bytes = delta.ops.iter().map(|op| op.payload.len()).sum::<usize>();
    assert!(payload_bytes < ValidationLimits::default().max_payload_bytes);
    let expected_vector = vault.session_mut("note.md").unwrap().state_vector();
    let expected = format!("a{}", "abcdef".repeat(size));
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        expected
    );
    if cfg!(feature = "perf_trace") {
        assert!(edit_stats.count(Span::SequenceOrderOracle) > 0);
    }
    report(
        profile,
        "vault-six-grapheme-edit",
        size,
        edit_wall_ns,
        &edit_stats,
    );
    println!(
        "{}",
        serde_json::json!({ "harness": "debug-ordering-v1", "size": size, "wire_operations": delta.ops.len(), "payload_bytes": payload_bytes, "rendered_bytes": expected.len() })
    );
    drop(delta);
    let start = Instant::now();
    let (_, flush_stats) = perf::measure(|| vault.flush_document("note.md").unwrap());
    let flush_wall_ns = start.elapsed().as_nanos();
    report(profile, "vault-flush", size, flush_wall_ns, &flush_stats);
    drop(vault);
    let start = Instant::now();
    let (mut reopened, open_stats) = perf::measure(|| {
        let mut reopened = VaultSession::open(root.path()).unwrap();
        reopened.open_document("note.md").unwrap();
        reopened
    });
    let open_wall_ns = start.elapsed().as_nanos();
    assert_eq!(
        reopened
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        expected
    );
    assert_eq!(
        reopened.session_mut("note.md").unwrap().state_vector(),
        expected_vector
    );
    report(
        profile,
        "vault-open-replay",
        size,
        open_wall_ns,
        &open_stats,
    );
    reopened
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 1 + size * 6, "!").unwrap();
        })
        .unwrap();
    drop(reopened);
    let mut reopened = VaultSession::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        format!("{expected}!")
    );
}

fn main() {
    if !cfg!(debug_assertions) {
        eprintln!("this debug-gate experiment requires all debug assertions");
        std::process::exit(2);
    }
    assert!(Sequence::<u8>::incremental_ordering_enabled());
    let mut args = std::env::args().skip(1);
    let profile = args
        .next()
        .expect("provide the recorded Cargo profile label as the first argument");
    let mut sizes: Vec<usize> = args
        .map(|arg| {
            arg.parse()
                .expect("workload sizes must be positive integers")
        })
        .collect();
    if sizes.is_empty() {
        sizes.extend([128, 512]);
    }
    for size in sizes {
        assert!(size > 0);
        mixed_sequence(&profile, size);
        vault_append_replay(&profile, size);
    }
}
