//! Mark operations for rich text formatting.
//!
//! This module provides operations for manipulating marks (formatting) on text,
//! including expansion during insert and splitting during remove.

use crate::core::mark::{
    Anchor, AnchorBias, MarkInterval, MarkIntervalId, MarkKind, MarkSet, MarkValue,
};
use crate::core::{LwwRegister, OpId, StateVector};
use std::collections::BTreeMap;

pub fn expand_marks_for_insert(
    mark_set: &MarkSet,
    element_order: &[OpId],
    visible_len: usize,
    anchor: Anchor,
    expand: bool,
) -> Vec<MarkIntervalId> {
    if !expand {
        return Vec::new();
    }
    let spans = mark_set.render_spans(element_order, visible_len);
    let index = resolve_anchor(&anchor, element_order, visible_len);
    for span in spans {
        if index >= span.start && index < span.end {
            return span.marks;
        }
    }
    Vec::new()
}

pub fn lower_remove_mark_range(
    mark_set: &MarkSet,
    interval_id: MarkIntervalId,
    remove_start: Anchor,
    remove_end: Anchor,
    next_id: OpId,
) -> (Vec<MarkInterval>, Vec<MarkIntervalId>) {
    let mut new_intervals = Vec::new();
    let mut removed = Vec::new();
    if !mark_set.is_active(&interval_id) {
        return (new_intervals, removed);
    }
    removed.push(interval_id);

    let base = mark_set
        .active_intervals()
        .into_iter()
        .find(|i| i.id == interval_id);
    let Some(interval) = base else {
        return (new_intervals, removed);
    };

    let left_needed = anchor_lt(interval.start, remove_start);
    let right_needed = anchor_lt(remove_end, interval.end);

    if left_needed {
        let mut attrs = BTreeMap::new();
        for (k, v) in &interval.attrs {
            attrs.insert(k.clone(), v.get());
        }
        new_intervals.push(MarkInterval {
            id: next_id,
            kind: interval.kind.clone(),
            start: interval.start,
            end: remove_start,
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k, LwwRegister::new(v, next_id)))
                .collect(),
            op_id: next_id,
        });
    }

    if right_needed {
        let mut attrs = BTreeMap::new();
        for (k, v) in &interval.attrs {
            attrs.insert(k.clone(), v.get());
        }
        let id = OpId {
            counter: next_id.counter + 1,
            peer: next_id.peer,
        };
        new_intervals.push(MarkInterval {
            id,
            kind: interval.kind.clone(),
            start: remove_end,
            end: interval.end,
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k, LwwRegister::new(v, id)))
                .collect(),
            op_id: id,
        });
    }

    (new_intervals, removed)
}

pub fn remove_mark_observed(
    interval_id: MarkIntervalId,
    observed: StateVector,
    op_id: OpId,
) -> (MarkIntervalId, StateVector, OpId) {
    (interval_id, observed, op_id)
}

fn resolve_anchor(anchor: &Anchor, order: &[OpId], len: usize) -> usize {
    let idx = order
        .iter()
        .position(|id| *id == anchor.elem_id)
        .unwrap_or(0);
    match anchor.bias {
        AnchorBias::Before => idx,
        AnchorBias::After => (idx + 1).min(len),
    }
}

fn anchor_lt(a: Anchor, b: Anchor) -> bool {
    (a.elem_id, bias_rank(a.bias)) < (b.elem_id, bias_rank(b.bias))
}

fn bias_rank(bias: AnchorBias) -> u8 {
    match bias {
        AnchorBias::Before => 0,
        AnchorBias::After => 1,
    }
}

pub fn sample_interval(id: OpId, start: OpId, end: OpId) -> MarkInterval {
    MarkInterval {
        id,
        kind: MarkKind::Bold,
        start: Anchor {
            elem_id: start,
            bias: AnchorBias::Before,
        },
        end: Anchor {
            elem_id: end,
            bias: AnchorBias::After,
        },
        attrs: BTreeMap::new(),
        op_id: id,
    }
}

pub fn mark_value_string(value: &str) -> MarkValue {
    MarkValue::String(value.to_string())
}
