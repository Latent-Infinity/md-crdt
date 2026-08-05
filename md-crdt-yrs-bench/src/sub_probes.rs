//! Optional md-crdt hot-path sub-probes (requires feature `sub_probes`).
//!
//! Uses product `perf_trace` counters to attribute wall time of competitive
//! insert and apply paths into exclusive spans. Never ratioed against Yrs.

#![cfg(feature = "sub_probes")]

use crate::sizes::{fill_text, keystroke_payload, middle_index};
use md_crdt::block_id_from_op;
use md_crdt::core::StateVector;
use md_crdt::perf::{self, PerfSnapshot, Span};
use md_crdt::session::CollaborativeDocument;
use md_crdt::sync::ValidationLimits;
use std::sync::{Mutex, MutexGuard};

/// Product counters are process-wide; serialize attribution helpers.
static ATTR_LOCK: Mutex<()> = Mutex::new(());

fn lock_attr() -> MutexGuard<'static, ()> {
    ATTR_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Named attribution row for reporting.
#[derive(Debug, Clone)]
pub struct SpanShare {
    pub span: &'static str,
    pub count: u64,
    pub nanos: u64,
    pub share: f64,
}

fn span_name(span: Span) -> &'static str {
    match span {
        Span::BlockLookup => "block_lookup",
        Span::UnitExpand => "unit_expand",
        Span::SequenceApply => "sequence_apply",
        Span::EnvelopeEncode => "envelope_encode",
        Span::SyncLogAppend => "sync_log_append",
        Span::ApplyValidate => "apply_validate",
        Span::ApplyDecode => "apply_decode",
        Span::ApplyIntegrate => "apply_integrate",
    }
}

/// Convert a snapshot into ordered share rows (highest nanos first).
pub fn shares(snap: &PerfSnapshot) -> Vec<SpanShare> {
    let all = [
        Span::BlockLookup,
        Span::UnitExpand,
        Span::SequenceApply,
        Span::EnvelopeEncode,
        Span::SyncLogAppend,
        Span::ApplyValidate,
        Span::ApplyDecode,
        Span::ApplyIntegrate,
    ];
    let mut rows: Vec<SpanShare> = all
        .into_iter()
        .map(|span| SpanShare {
            span: span_name(span),
            count: snap.count(span),
            nanos: snap.nanos(span),
            share: snap.share(span),
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.nanos));
    rows
}

fn seed_paragraph(peer: u64, text: &str) -> (CollaborativeDocument, md_crdt::BlockId) {
    let mut session = CollaborativeDocument::new(peer);
    let elem = session.insert_paragraph(None, text).expect("seed");
    let block_id = block_id_from_op(elem);
    (session, block_id)
}

/// Attribute one middle insert of `"y"` into a bulk-seeded document of length `n`.
pub fn attribute_insert_middle(n: usize) -> (PerfSnapshot, u64) {
    let _guard = lock_attr();
    let (base, block_id) = seed_paragraph(1, &fill_text(n));
    let snapshot = base.save_snapshot().expect("snapshot");

    let wall = perf::wall_nanos(|| {
        let mut session =
            CollaborativeDocument::restore_from_snapshot(snapshot.clone()).expect("restore");
        let _ = session
            .insert_text(block_id, middle_index(n), "y")
            .expect("insert");
    });

    let (_, snap) = perf::measure(|| {
        let mut session = CollaborativeDocument::restore_from_snapshot(snapshot).expect("restore");
        let _ = session
            .insert_text(block_id, middle_index(n), "y")
            .expect("insert");
    });
    (snap, wall)
}

/// Attribute applying a full document of bulk seed `n` plus `k` lag keystrokes onto an empty peer.
pub fn attribute_apply_remote_lag(n: usize, k: usize) -> (PerfSnapshot, u64) {
    let _guard = lock_attr();
    let (mut source, block_id) = seed_paragraph(1, &fill_text(n));
    for i in 0..k {
        source
            .insert_text(block_id, n.saturating_add(i), keystroke_payload())
            .expect("keystroke");
    }
    let message = source
        .encode_changes_since(&StateVector::default())
        .expect("encode");

    let wall = perf::wall_nanos(|| {
        let mut peer = CollaborativeDocument::new(2);
        peer.apply_remote(message.clone(), &ValidationLimits::default())
            .expect("apply");
    });
    let (_, snap) = perf::measure(|| {
        let mut peer = CollaborativeDocument::new(2);
        peer.apply_remote(message, &ValidationLimits::default())
            .expect("apply");
    });
    (snap, wall)
}

/// Format a human-readable markdown table of span shares.
pub fn format_attribution_table(title: &str, snap: &PerfSnapshot, wall_nanos: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!("### {title}\n\n"));
    out.push_str(&format!(
        "Wall (approx): {:.3} ms · instrumented total: {:.3} ms\n\n",
        wall_nanos as f64 / 1e6,
        snap.total_nanos() as f64 / 1e6
    ));
    out.push_str("| Span | Count | ns | Share |\n| --- | ---: | ---: | ---: |\n");
    for row in shares(snap) {
        if row.count == 0 && row.nanos == 0 {
            continue;
        }
        out.push_str(&format!(
            "| `{}` | {} | {} | {:.1}% |\n",
            row.span,
            row.count,
            row.nanos,
            row.share * 100.0
        ));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_middle_records_local_spans() {
        let (snap, wall) = attribute_insert_middle(128);
        assert!(wall > 0);
        assert_eq!(snap.count(Span::BlockLookup), 1);
        assert_eq!(snap.count(Span::UnitExpand), 1);
        assert_eq!(snap.count(Span::EnvelopeEncode), 1);
        assert_eq!(snap.count(Span::SequenceApply), 1);
        assert_eq!(snap.count(Span::SyncLogAppend), 1);
        assert!(snap.total_nanos() > 0);
    }

    #[test]
    fn apply_remote_lag_records_remote_spans() {
        let (snap, wall) = attribute_apply_remote_lag(64, 4);
        assert!(wall > 0);
        assert!(snap.count(Span::ApplyValidate) >= 1);
        assert!(snap.count(Span::ApplyDecode) >= 1);
        assert!(snap.count(Span::ApplyIntegrate) >= 1);
    }

    #[test]
    fn format_table_includes_title() {
        let (snap, wall) = attribute_insert_middle(32);
        let table = format_attribution_table("insert n=32", &snap, wall);
        assert!(table.contains("insert n=32"));
        assert!(table.contains("block_lookup"));
    }
}
