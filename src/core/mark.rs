//! Rich text mark/formatting system.
//!
//! This module provides a CRDT-based mark system for rich text formatting,
//! supporting operations like bold, italic, links, and custom marks.

use super::{LwwRegister, OpId, StateVector};
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

    /// Active intervals resolved to half-open visible grapheme ranges.
    pub fn resolved_intervals(&self, element_order: &[OpId]) -> Vec<(&MarkInterval, usize, usize)> {
        let index_map: BTreeMap<OpId, usize> = element_order
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let len = element_order.len();
        self.iter_active_intervals()
            .map(|interval| {
                let start = resolve_anchor(&interval.start, &index_map, len);
                let end = resolve_anchor(&interval.end, &index_map, len);
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

fn resolve_anchor(anchor: &Anchor, index_map: &BTreeMap<OpId, usize>, len: usize) -> usize {
    let base = index_map.get(&anchor.elem_id).copied().unwrap_or(0);
    match anchor.bias {
        AnchorBias::Before => base,
        AnchorBias::After => (base + 1).min(len),
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
