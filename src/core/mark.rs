//! Rich text mark/formatting system.
//!
//! This module provides a CRDT-based mark system for rich text formatting,
//! supporting operations like bold, italic, links, and custom marks.

use super::{LwwRegister, OpId, Sequence, StateVector};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MarkKind {
    Bold,
    Italic,
    Code,
    Link,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MarkValue {
    String(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorBias {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub elem_id: OpId,
    pub bias: AnchorBias,
}

pub type MarkIntervalId = OpId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkInterval {
    pub id: MarkIntervalId,
    pub kind: MarkKind,
    pub start: Anchor,
    pub end: Anchor,
    pub attrs: BTreeMap<String, LwwRegister<MarkValue>>,
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMark {
    pub observed: StateVector,
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkSet {
    intervals: BTreeMap<MarkIntervalId, MarkInterval>,
    removes: BTreeMap<MarkIntervalId, RemoveMark>,
}

#[derive(Serialize, Deserialize)]
struct MarkSetSerde {
    intervals: Vec<MarkInterval>,
    removes: Vec<(MarkIntervalId, RemoveMark)>,
}

impl Serialize for MarkSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        MarkSetSerde {
            intervals: self.intervals.values().cloned().collect(),
            removes: self
                .removes
                .iter()
                .map(|(id, remove)| (*id, remove.clone()))
                .collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MarkSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = MarkSetSerde::deserialize(deserializer)?;
        Ok(Self {
            intervals: value
                .intervals
                .into_iter()
                .map(|interval| (interval.id, interval))
                .collect(),
            removes: value.removes.into_iter().collect(),
        })
    }
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

    /// Merge mark history from another text block when its units are appended here.
    pub(crate) fn merge_from(&mut self, other: &Self) {
        for (id, interval) in &other.intervals {
            match self.intervals.get(id) {
                Some(existing) if existing.op_id >= interval.op_id => {}
                _ => {
                    self.intervals.insert(*id, interval.clone());
                }
            }
        }
        for (id, remove) in &other.removes {
            match self.removes.get(id) {
                Some(existing) if existing.op_id >= remove.op_id => {}
                _ => {
                    self.removes.insert(*id, remove.clone());
                }
            }
        }
    }

    /// Look up an interval by id (active or not).
    pub fn interval(&self, interval_id: &MarkIntervalId) -> Option<&MarkInterval> {
        self.intervals.get(interval_id)
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

    pub(crate) fn iter_all_intervals(&self) -> impl Iterator<Item = &MarkInterval> {
        self.intervals.values()
    }

    pub(crate) fn iter_removes(&self) -> impl Iterator<Item = (&MarkIntervalId, &RemoveMark)> {
        self.removes.iter()
    }

    pub fn render_spans(&self, element_order: &[OpId], visible_len: usize) -> Vec<Span> {
        let offsets = visible_anchor_offsets(element_order);
        self.render_spans_with_offsets(&offsets, visible_len)
    }

    /// Active intervals resolved to half-open visible grapheme ranges.
    pub fn resolved_intervals(&self, element_order: &[OpId]) -> Vec<(&MarkInterval, usize, usize)> {
        let offsets = visible_anchor_offsets(element_order);
        self.resolved_intervals_with_offsets(&offsets, element_order.len())
    }

    /// Render mark spans while preserving anchors that point at tombstoned elements.
    pub fn render_spans_in_sequence<T: Clone>(&self, sequence: &Sequence<T>) -> Vec<Span> {
        let (offsets, visible_len) = sequence_anchor_offsets(sequence);
        self.render_spans_with_offsets(&offsets, visible_len)
    }

    /// Resolve active intervals while preserving anchors that point at tombstoned elements.
    pub fn resolved_intervals_in_sequence<T: Clone>(
        &self,
        sequence: &Sequence<T>,
    ) -> Vec<(&MarkInterval, usize, usize)> {
        let (offsets, visible_len) = sequence_anchor_offsets(sequence);
        self.resolved_intervals_with_offsets(&offsets, visible_len)
    }

    fn render_spans_with_offsets(&self, offsets: &AnchorOffsets, visible_len: usize) -> Vec<Span> {
        let mut marks_at: Vec<Vec<MarkIntervalId>> = vec![Vec::new(); visible_len + 1];
        for interval in self.iter_active_intervals() {
            let start = resolve_anchor(&interval.start, offsets, visible_len);
            let end = resolve_anchor(&interval.end, offsets, visible_len);
            let (from, to) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            for marks in marks_at.iter_mut().take(to).skip(from) {
                marks.push(interval.id);
            }
        }

        for marks in &mut marks_at {
            marks.sort();
            marks.dedup();
        }

        let mut spans = Vec::with_capacity(visible_len.min(64));
        let mut start = 0usize;
        while start < visible_len {
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

    fn resolved_intervals_with_offsets(
        &self,
        offsets: &AnchorOffsets,
        visible_len: usize,
    ) -> Vec<(&MarkInterval, usize, usize)> {
        self.iter_active_intervals()
            .map(|interval| {
                let start = resolve_anchor(&interval.start, offsets, visible_len);
                let end = resolve_anchor(&interval.end, offsets, visible_len);
                (interval, start.min(end), start.max(end))
            })
            .collect()
    }
}

impl Default for MarkSet {
    fn default() -> Self {
        Self::new()
    }
}

type AnchorOffsets = BTreeMap<OpId, (usize, usize)>;

fn visible_anchor_offsets(element_order: &[OpId]) -> AnchorOffsets {
    element_order
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, (index, index + 1)))
        .collect()
}

fn sequence_anchor_offsets<T: Clone>(sequence: &Sequence<T>) -> (AnchorOffsets, usize) {
    let elements: Vec<_> = sequence.iter_all().collect();
    let mut visible_offset = 0usize;
    let mut offsets = BTreeMap::new();
    let mut insertion_offsets = BTreeMap::new();

    for element in &elements {
        let before = visible_offset;
        let insertion_offset = if element.value.is_some() {
            visible_offset += 1;
            visible_offset
        } else {
            element
                .after
                .and_then(|parent| insertion_offsets.get(&parent).copied())
                .unwrap_or(0)
        };
        let anchor_before = if element.value.is_some() {
            before
        } else {
            insertion_offset
        };
        let anchor_after = if element.value.is_some() {
            visible_offset
        } else {
            anchor_before
        };
        offsets.insert(element.id, (anchor_before, anchor_after));
        insertion_offsets.insert(element.id, insertion_offset);
    }

    let mut descendant_offsets = BTreeMap::new();
    for element in elements.iter().rev() {
        let candidate = if element.value.is_some() {
            offsets.get(&element.id).map(|(before, _)| *before)
        } else {
            descendant_offsets.get(&element.id).copied()
        };
        if let (Some(parent), Some(candidate)) = (element.after, candidate) {
            descendant_offsets
                .entry(parent)
                .and_modify(|offset: &mut usize| *offset = (*offset).min(candidate))
                .or_insert(candidate);
        }
    }

    for element in elements {
        if element.value.is_none()
            && let Some(after) = descendant_offsets.get(&element.id)
            && let Some((_, anchor_after)) = offsets.get_mut(&element.id)
        {
            *anchor_after = *after;
        }
    }

    (offsets, visible_offset)
}

fn resolve_anchor(anchor: &Anchor, offsets: &AnchorOffsets, len: usize) -> usize {
    let fallback_after = usize::from(len > 0);
    let (before, after) = offsets
        .get(&anchor.elem_id)
        .copied()
        .unwrap_or((0, fallback_after));
    match anchor.bias {
        AnchorBias::Before => before.min(len),
        AnchorBias::After => after.min(len),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(counter: u64) -> OpId {
        OpId { counter, peer: 1 }
    }

    fn anchor(counter: u64) -> Anchor {
        Anchor {
            elem_id: op(counter),
            bias: AnchorBias::Before,
        }
    }

    #[test]
    fn merge_from_keeps_newest_mark_and_remove_history() {
        let mut left = MarkSet::new();
        left.set_mark(
            op(1),
            MarkKind::Bold,
            anchor(10),
            anchor(11),
            BTreeMap::new(),
            op(9),
        );
        left.remove_mark(op(1), StateVector::new(), op(9));

        let mut right = MarkSet::new();
        right.set_mark(
            op(1),
            MarkKind::Italic,
            anchor(10),
            anchor(11),
            BTreeMap::new(),
            op(8),
        );
        right.remove_mark(op(1), StateVector::new(), op(8));
        right.set_mark(
            op(2),
            MarkKind::Code,
            anchor(12),
            anchor(13),
            BTreeMap::new(),
            op(10),
        );
        right.remove_mark(op(2), StateVector::new(), op(10));

        left.merge_from(&right);

        assert_eq!(
            left.interval(&op(1)).expect("existing mark").kind,
            MarkKind::Bold
        );
        assert_eq!(
            left.interval(&op(2)).expect("merged mark").kind,
            MarkKind::Code
        );
        assert_eq!(
            left.removes.get(&op(1)).expect("existing remove").op_id,
            op(9)
        );
        assert_eq!(
            left.removes.get(&op(2)).expect("merged remove").op_id,
            op(10)
        );
    }
}
