//! Rich text mark/formatting system.
//!
//! This module provides a CRDT-based mark system for rich text formatting,
//! supporting operations like bold, italic, links, and custom marks.

use super::{LwwRegister, OpId, StateVector};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MarkKind {
    Bold,
    Italic,
    Code,
    Link,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkValue {
    String(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorBias {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub elem_id: OpId,
    pub bias: AnchorBias,
}

pub type MarkIntervalId = OpId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkInterval {
    pub id: MarkIntervalId,
    pub kind: MarkKind,
    pub start: Anchor,
    pub end: Anchor,
    pub attrs: BTreeMap<String, LwwRegister<MarkValue>>,
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveMark {
    pub observed: StateVector,
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkSet {
    intervals: BTreeMap<MarkIntervalId, MarkInterval>,
    removes: BTreeMap<MarkIntervalId, RemoveMark>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub marks: Vec<MarkIntervalId>,
}

impl MarkSet {
    pub fn new() -> Self {
        Self {
            intervals: BTreeMap::new(),
            removes: BTreeMap::new(),
        }
    }

    pub fn set_mark(
        &mut self,
        interval_id: MarkIntervalId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
        attrs: BTreeMap<String, MarkValue>,
        op_id: OpId,
    ) {
        let entry = self
            .intervals
            .entry(interval_id)
            .or_insert_with(|| MarkInterval {
                id: interval_id,
                kind: kind.clone(),
                start,
                end,
                attrs: BTreeMap::new(),
                op_id,
            });

        if op_id >= entry.op_id {
            entry.kind = kind;
            entry.start = start;
            entry.end = end;
            entry.op_id = op_id;
        }

        for (key, value) in attrs {
            entry
                .attrs
                .entry(key)
                .and_modify(|reg| reg.set(value.clone(), op_id))
                .or_insert_with(|| LwwRegister::new(value, op_id));
        }
    }

    pub fn remove_mark(&mut self, interval_id: MarkIntervalId, observed: StateVector, op_id: OpId) {
        match self.removes.get(&interval_id) {
            Some(existing) if existing.op_id >= op_id => {}
            _ => {
                self.removes
                    .insert(interval_id, RemoveMark { observed, op_id });
            }
        }
    }

    pub fn is_active(&self, interval_id: &MarkIntervalId) -> bool {
        let Some(interval) = self.intervals.get(interval_id) else {
            return false;
        };
        self.removes.get(interval_id).is_none_or(|remove| {
            let seen = remove.observed.get(interval.id.peer).unwrap_or(0);
            seen < interval.id.counter
        })
    }

    /// Returns a Vec of active intervals. Use `iter_active_intervals()` for lazy iteration.
    pub fn active_intervals(&self) -> Vec<&MarkInterval> {
        self.intervals
            .values()
            .filter(|interval| self.is_active(&interval.id))
            .collect()
    }

    /// Returns an iterator over active intervals (lazy, no allocation).
    /// Prefer this over `active_intervals()` when you don't need indexing.
    #[inline]
    pub fn iter_active_intervals(&self) -> impl Iterator<Item = &MarkInterval> {
        self.intervals
            .values()
            .filter(|interval| self.is_active(&interval.id))
    }

    pub fn render_spans(&self, element_order: &[OpId], visible_len: usize) -> Vec<Span> {
        let mut index_map: BTreeMap<OpId, usize> = BTreeMap::new();
        for (visible_index, id) in element_order.iter().enumerate() {
            index_map.insert(*id, visible_index);
        }

        let mut marks_at: Vec<Vec<MarkIntervalId>> = vec![Vec::new(); visible_len + 1];
        for interval in self.iter_active_intervals() {
            let start = resolve_anchor(&interval.start, &index_map, visible_len);
            let end = resolve_anchor(&interval.end, &index_map, visible_len);
            let (from, to) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            for idx in from..to {
                if idx < marks_at.len() {
                    marks_at[idx].push(interval.id);
                }
            }
        }

        for marks in &mut marks_at {
            marks.sort();
            marks.dedup();
        }

        // Pre-allocate spans - worst case is one span per position
        let mut spans = Vec::with_capacity(visible_len.min(64));
        let mut start = 0usize;
        while start < visible_len {
            // Use std::mem::take to move instead of clone where possible
            let current = std::mem::take(&mut marks_at[start]);
            let mut end = start + 1;
            while end < visible_len && marks_at[end] == current {
                end += 1;
            }
            spans.push(Span {
                start,
                end,
                marks: current,
            });
            start = end;
        }

        spans
    }
}

impl Default for MarkSet {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_anchor(anchor: &Anchor, index_map: &BTreeMap<OpId, usize>, len: usize) -> usize {
    let base = index_map.get(&anchor.elem_id).copied().unwrap_or(0);
    match anchor.bias {
        AnchorBias::Before => base,
        AnchorBias::After => (base + 1).min(len),
    }
}
