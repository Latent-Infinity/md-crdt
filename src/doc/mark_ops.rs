//! Mark operations for rich text formatting.
//!
//! This module provides operations for manipulating marks (formatting) on text,
//! including expansion during insert and splitting during remove.

use crate::core::mark::{Anchor, AnchorBias, MarkInterval, MarkIntervalId, MarkSet};
use crate::core::{LwwRegister, OpId};
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
    element_order: &[OpId],
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

    // Compare anchors by visible text position, not raw OpId: RGA can order
    // concurrently-inserted units so that OpId order differs from visible order.
    let len = element_order.len();
    let pos = |a: &Anchor| resolve_anchor(a, element_order, len);
    let left_needed = pos(&interval.start) < pos(&remove_start);
    let right_needed = pos(&remove_end) < pos(&interval.end);

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
