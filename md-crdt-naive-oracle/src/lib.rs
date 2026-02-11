//! A naive, simple oracle implementation for differential testing.
use md_crdt::core::{OpId, SequenceOp, StateVector};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Element<T> {
    id: OpId,
    value: Option<T>,
    after: Option<OpId>,
    right_origin: Option<OpId>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sequence<T> {
    elements: Vec<Element<T>>,
}

impl<T: Clone> Sequence<T> {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
        }
    }

    pub fn apply(&mut self, op: SequenceOp<T>) {
        match op {
            SequenceOp::Insert {
                after,
                id,
                value,
                right_origin,
            } => self.insert(after, value, id, right_origin),
            SequenceOp::Delete { target, .. } => self.delete(target),
        }
    }

    pub fn insert(&mut self, after: Option<OpId>, value: T, id: OpId, right_origin: Option<OpId>) {
        if self.elements.iter().any(|elem| elem.id == id) {
            return;
        }

        self.elements.insert(
            0,
            Element {
                id,
                value: Some(value),
                after,
                right_origin,
            },
        );
        self.rebuild_order();
    }

    pub fn delete(&mut self, target: OpId) {
        if let Some(elem) = self.elements.iter_mut().find(|elem| elem.id == target) {
            elem.value = None;
        }
    }

    pub fn elements(&self) -> Vec<T> {
        self.elements
            .iter()
            .filter_map(|elem| elem.value.clone())
            .collect()
    }

    fn rebuild_order(&mut self) {
        let mut element_map: BTreeMap<OpId, Element<T>> = BTreeMap::new();
        for elem in self.elements.drain(..) {
            element_map.insert(elem.id, elem);
        }

        let mut children: BTreeMap<Option<OpId>, Vec<OpId>> = BTreeMap::new();
        for elem in element_map.values() {
            children.entry(elem.after).or_default().push(elem.id);
        }

        for ids in children.values_mut() {
            ids.sort_by(|a, b| {
                let elem_a = element_map.get(a).unwrap();
                let elem_b = element_map.get(b).unwrap();
                match (elem_a.right_origin, elem_b.right_origin) {
                    (Some(ra), Some(rb)) => {
                        if ra == rb {
                            b.cmp(a)
                        } else {
                            ra.cmp(&rb)
                        }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => b.cmp(a),
                }
            });
        }

        let mut ordered_ids = Vec::with_capacity(element_map.len());
        Self::walk_children(None, &children, &mut ordered_ids);

        self.elements = ordered_ids
            .into_iter()
            .filter_map(|id| element_map.get(&id).cloned())
            .collect();
    }

    fn walk_children(
        parent: Option<OpId>,
        children: &BTreeMap<Option<OpId>, Vec<OpId>>,
        out: &mut Vec<OpId>,
    ) {
        if let Some(kids) = children.get(&parent) {
            for id in kids {
                out.push(*id);
                Self::walk_children(Some(*id), children, out);
            }
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOracle {
    ops: BTreeMap<OpId, Vec<u8>>,
}

impl SyncOracle {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, id: OpId, payload: Vec<u8>) {
        self.ops.entry(id).or_insert(payload);
    }

    pub fn state_vector(&self) -> StateVector {
        let mut sv = StateVector::new();
        for id in self.ops.keys() {
            let current = sv.get(id.peer).unwrap_or(0);
            if id.counter > current {
                sv.set(id.peer, id.counter);
            }
        }
        sv
    }

    pub fn changes_since(&self, since: &StateVector) -> Vec<(OpId, Vec<u8>)> {
        let mut changes = Vec::new();
        for (id, payload) in &self.ops {
            let seen = since.get(id.peer).unwrap_or(0);
            if id.counter > seen {
                changes.push((*id, payload.clone()));
            }
        }
        changes
    }

    pub fn same_state(&self, other: &Self) -> bool {
        self.ops == other.ops
    }
}

pub mod mark {
    use md_crdt::core::mark::{Anchor, AnchorBias, MarkIntervalId, MarkKind, MarkValue, Span};
    use md_crdt::core::{OpId, StateVector};
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NaiveMarkInterval {
        pub id: MarkIntervalId,
        pub kind: MarkKind,
        pub start: Anchor,
        pub end: Anchor,
        pub attrs: BTreeMap<String, MarkValue>,
        pub op_id: OpId,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NaiveMarkSet {
        intervals: BTreeMap<MarkIntervalId, NaiveMarkInterval>,
        removes: BTreeMap<MarkIntervalId, (StateVector, OpId)>,
    }

    impl Default for NaiveMarkSet {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NaiveMarkSet {
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
                .or_insert(NaiveMarkInterval {
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

            for (k, v) in attrs {
                entry.attrs.insert(k, v);
            }
        }

        pub fn remove_mark(
            &mut self,
            interval_id: MarkIntervalId,
            observed: StateVector,
            op_id: OpId,
        ) {
            match self.removes.get(&interval_id) {
                Some((_, existing)) if *existing >= op_id => {}
                _ => {
                    self.removes.insert(interval_id, (observed, op_id));
                }
            }
        }

        pub fn is_active(&self, interval_id: &MarkIntervalId) -> bool {
            let Some(interval) = self.intervals.get(interval_id) else {
                return false;
            };
            match self.removes.get(interval_id) {
                None => true,
                Some((observed, _)) => {
                    let seen = observed.get(interval.id.peer).unwrap_or(0);
                    seen < interval.id.counter
                }
            }
        }

        pub fn render_spans(&self, element_order: &[OpId], visible_len: usize) -> Vec<Span> {
            let mut index_map: BTreeMap<OpId, usize> = BTreeMap::new();
            for (visible_index, id) in element_order.iter().enumerate() {
                index_map.insert(*id, visible_index);
            }

            let mut marks_at: Vec<Vec<MarkIntervalId>> = vec![Vec::new(); visible_len + 1];
            for interval in self.intervals.values() {
                if !self.is_active(&interval.id) {
                    continue;
                }
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

            let mut spans = Vec::new();
            let mut start = 0usize;
            while start < visible_len {
                let current = marks_at[start].clone();
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

    fn resolve_anchor(anchor: &Anchor, index_map: &BTreeMap<OpId, usize>, len: usize) -> usize {
        let base = index_map.get(&anchor.elem_id).copied().unwrap_or(0);
        match anchor.bias {
            AnchorBias::Before => base,
            AnchorBias::After => (base + 1).min(len),
        }
    }
}
