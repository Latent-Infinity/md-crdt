use md_crdt::core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
use md_crdt::core::{OpId, StateVector};
use std::collections::BTreeMap;

fn op(peer: u64, counter: u64) -> OpId {
    OpId { counter, peer }
}

#[test]
fn test_set_mark_and_lww_attrs() {
    let mut set = MarkSet::new();
    let id = op(1, 1);
    let start = Anchor {
        elem_id: op(1, 10),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 11),
        bias: AnchorBias::After,
    };

    let mut attrs = BTreeMap::new();
    attrs.insert("href".to_string(), MarkValue::String("a".into()));
    set.set_mark(id, MarkKind::Link, start, end, attrs, op(1, 1));

    let mut attrs2 = BTreeMap::new();
    attrs2.insert("href".to_string(), MarkValue::String("b".into()));
    set.set_mark(id, MarkKind::Link, start, end, attrs2, op(1, 2));

    let interval = set.active_intervals()[0];
    let href = interval.attrs.get("href").unwrap().get();
    assert_eq!(href, MarkValue::String("b".into()));
}

#[test]
fn test_remove_mark_causal_add_wins() {
    let mut set = MarkSet::new();
    let id = op(1, 1);
    let start = Anchor {
        elem_id: op(1, 10),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 11),
        bias: AnchorBias::After,
    };
    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), op(1, 1));

    let mut observed = StateVector::new();
    observed.set(1, 0);
    set.remove_mark(id, observed, op(2, 1));
    assert!(set.is_active(&id));

    let mut observed2 = StateVector::new();
    observed2.set(1, 1);
    set.remove_mark(id, observed2, op(2, 2));
    assert!(!set.is_active(&id));
}

#[test]
fn test_span_rendering_deterministic() {
    let mut set = MarkSet::new();
    let id = op(1, 1);
    let start = Anchor {
        elem_id: op(1, 1),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 3),
        bias: AnchorBias::After,
    };
    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), op(1, 1));

    let order = vec![op(1, 1), op(1, 2), op(1, 3)];
    let spans_a = set.render_spans(&order, 3);
    let spans_b = set.render_spans(&order, 3);
    assert_eq!(spans_a, spans_b);
}

#[test]
fn test_span_invariants_partition() {
    let mut set = MarkSet::new();
    let id = op(1, 1);
    let start = Anchor {
        elem_id: op(1, 1),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 2),
        bias: AnchorBias::After,
    };
    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), op(1, 1));

    let order = vec![op(1, 1), op(1, 2)];
    let spans = set.render_spans(&order, 2);
    assert_eq!(spans.first().unwrap().start, 0);
    assert_eq!(spans.last().unwrap().end, 2);
}
