//! Tests for INV-6 (Mark Interval Invariants) and INV-2 (Span Rendering Invariants)

use md_crdt_core::OpId;
use md_crdt_core::mark::{Anchor, AnchorBias, MarkKind, MarkSet};
use std::collections::{BTreeMap, HashSet};

fn op(peer: u64, counter: u64) -> OpId {
    OpId { counter, peer }
}

// INV-6: MarkIntervalId is unique within a document
#[test]
fn inv6_unique_mark_interval_ids() {
    let mut set = MarkSet::new();
    let id1 = op(1, 1);
    let id2 = op(1, 2);
    let id3 = op(2, 1);

    let start = Anchor {
        elem_id: op(1, 10),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 11),
        bias: AnchorBias::After,
    };

    set.set_mark(id1, MarkKind::Bold, start, end, BTreeMap::new(), id1);
    set.set_mark(id2, MarkKind::Italic, start, end, BTreeMap::new(), id2);
    set.set_mark(id3, MarkKind::Code, start, end, BTreeMap::new(), id3);

    let active = set.active_intervals();
    let ids: HashSet<_> = active.iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), 3, "All three marks should have unique IDs");
}

// INV-6: Anchors reference valid ElemIds (existing or tombstone)
#[test]
fn inv6_anchors_reference_elements() {
    let mut set = MarkSet::new();
    let id = op(1, 1);

    // Create anchors that reference specific element IDs
    let elem1 = op(1, 10);
    let elem2 = op(1, 11);

    let start = Anchor {
        elem_id: elem1,
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: elem2,
        bias: AnchorBias::After,
    };

    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

    let interval = set.active_intervals()[0];
    assert_eq!(interval.start.elem_id, elem1);
    assert_eq!(interval.end.elem_id, elem2);
}

// INV-6: Empty intervals (start == end) are valid
#[test]
fn inv6_empty_intervals_valid() {
    let mut set = MarkSet::new();
    let id = op(1, 1);
    let elem = op(1, 10);

    // Create an empty interval where start == end
    let anchor = Anchor {
        elem_id: elem,
        bias: AnchorBias::Before,
    };

    set.set_mark(id, MarkKind::Bold, anchor, anchor, BTreeMap::new(), id);

    assert!(
        set.is_active(&id),
        "Empty interval should be valid and active"
    );
    let interval = set.active_intervals()[0];
    assert_eq!(interval.start, interval.end);
}

// INV-2: Spans partition text without gaps or overlaps
#[test]
fn inv2_spans_partition_without_gaps() {
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

    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

    let order = vec![op(1, 1), op(1, 2), op(1, 3), op(1, 4)];
    let spans = set.render_spans(&order, 4);

    // Verify no gaps: each span's start should equal previous span's end
    for i in 1..spans.len() {
        assert_eq!(
            spans[i].start,
            spans[i - 1].end,
            "Spans should be contiguous without gaps"
        );
    }

    // Verify first span starts at 0
    if !spans.is_empty() {
        assert_eq!(spans[0].start, 0);
    }
}

// INV-2: Empty spans have empty marks list
#[test]
fn inv2_empty_spans_have_no_marks() {
    let mut set = MarkSet::new();
    let id = op(1, 1);

    // Mark only elements 1-2, leaving 3-4 unmarked
    let start = Anchor {
        elem_id: op(1, 1),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 2),
        bias: AnchorBias::After,
    };

    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

    let order = vec![op(1, 1), op(1, 2), op(1, 3), op(1, 4)];
    let spans = set.render_spans(&order, 4);

    // Find spans outside the marked region
    for span in &spans {
        if span.start >= 2 {
            assert!(
                span.marks.is_empty(),
                "Spans outside marked region should have empty marks"
            );
        }
    }
}

// DC-3: Overlapping marks of same kind rendered deterministically
#[test]
fn dc3_overlapping_same_kind_deterministic() {
    let mut set1 = MarkSet::new();
    let mut set2 = MarkSet::new();

    let id1 = op(1, 1);
    let id2 = op(2, 1);

    // Create two overlapping bold marks
    let start1 = Anchor {
        elem_id: op(1, 1),
        bias: AnchorBias::Before,
    };
    let end1 = Anchor {
        elem_id: op(1, 3),
        bias: AnchorBias::After,
    };
    let start2 = Anchor {
        elem_id: op(1, 2),
        bias: AnchorBias::Before,
    };
    let end2 = Anchor {
        elem_id: op(1, 4),
        bias: AnchorBias::After,
    };

    // Apply in different orders
    set1.set_mark(id1, MarkKind::Bold, start1, end1, BTreeMap::new(), id1);
    set1.set_mark(id2, MarkKind::Bold, start2, end2, BTreeMap::new(), id2);

    set2.set_mark(id2, MarkKind::Bold, start2, end2, BTreeMap::new(), id2);
    set2.set_mark(id1, MarkKind::Bold, start1, end1, BTreeMap::new(), id1);

    let order = vec![op(1, 1), op(1, 2), op(1, 3), op(1, 4)];
    let spans1 = set1.render_spans(&order, 4);
    let spans2 = set2.render_spans(&order, 4);

    assert_eq!(
        spans1, spans2,
        "Overlapping marks should render deterministically"
    );
}

// DC-3: Nested marks of different kinds rendered in consistent order
#[test]
fn dc3_nested_different_kinds_consistent() {
    let mut set1 = MarkSet::new();
    let mut set2 = MarkSet::new();

    let bold_id = op(1, 1);
    let italic_id = op(2, 1);

    // Bold covers [0, 4), Italic covers [1, 3) - nested
    let bold_start = Anchor {
        elem_id: op(1, 1),
        bias: AnchorBias::Before,
    };
    let bold_end = Anchor {
        elem_id: op(1, 4),
        bias: AnchorBias::After,
    };
    let italic_start = Anchor {
        elem_id: op(1, 2),
        bias: AnchorBias::Before,
    };
    let italic_end = Anchor {
        elem_id: op(1, 3),
        bias: AnchorBias::After,
    };

    // Apply in different orders
    set1.set_mark(
        bold_id,
        MarkKind::Bold,
        bold_start,
        bold_end,
        BTreeMap::new(),
        bold_id,
    );
    set1.set_mark(
        italic_id,
        MarkKind::Italic,
        italic_start,
        italic_end,
        BTreeMap::new(),
        italic_id,
    );

    set2.set_mark(
        italic_id,
        MarkKind::Italic,
        italic_start,
        italic_end,
        BTreeMap::new(),
        italic_id,
    );
    set2.set_mark(
        bold_id,
        MarkKind::Bold,
        bold_start,
        bold_end,
        BTreeMap::new(),
        bold_id,
    );

    let order = vec![op(1, 1), op(1, 2), op(1, 3), op(1, 4)];
    let spans1 = set1.render_spans(&order, 4);
    let spans2 = set2.render_spans(&order, 4);

    assert_eq!(
        spans1, spans2,
        "Nested marks should render in consistent order"
    );

    // Verify the middle span has both marks
    let middle_span = spans1.iter().find(|s| s.start == 1 && s.end == 3);
    assert!(middle_span.is_some());
    assert_eq!(middle_span.unwrap().marks.len(), 2);
}

// DC-3: Span boundaries align with mark anchors exactly
#[test]
fn dc3_span_boundaries_align_with_anchors() {
    let mut set = MarkSet::new();
    let id = op(1, 1);

    let start = Anchor {
        elem_id: op(1, 2),
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: op(1, 3),
        bias: AnchorBias::After,
    };

    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

    let order = vec![op(1, 1), op(1, 2), op(1, 3), op(1, 4)];
    let spans = set.render_spans(&order, 4);

    // Find the bold span
    let bold_span = spans.iter().find(|s| s.marks.contains(&id));
    assert!(bold_span.is_some());
    let bold_span = bold_span.unwrap();

    // The mark starts at element index 1 (op(1,2) is at index 1)
    // and ends at element index 3 (after op(1,3) which is at index 2)
    assert_eq!(bold_span.start, 1, "Span should start at anchor position");
    assert_eq!(bold_span.end, 3, "Span should end at anchor position");
}

// INV-2: Mark sets are valid (each mark has corresponding active interval)
#[test]
fn inv2_mark_sets_have_active_intervals() {
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

    set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

    let order = vec![op(1, 1), op(1, 2)];
    let spans = set.render_spans(&order, 2);

    // For each span with marks, verify those marks are active
    for span in &spans {
        for mark_id in &span.marks {
            assert!(
                set.is_active(mark_id),
                "Each mark in a span should be an active interval"
            );
        }
    }
}
