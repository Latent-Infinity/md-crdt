use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type PeerId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpId {
    pub counter: u64,
    pub peer: PeerId,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateVector {
    peers: BTreeMap<PeerId, u64>,
}

impl StateVector {
    pub fn new() -> Self {
        Self {
            peers: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn get(&self, peer: PeerId) -> Option<u64> {
        self.peers.get(&peer).copied()
    }

    pub fn set(&mut self, peer: PeerId, counter: u64) {
        self.peers.insert(peer, counter);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element<T> {
    pub id: OpId,
    pub value: Option<T>,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceOp<T> {
    Insert {
        after: Option<OpId>,
        id: OpId,
        value: T,
        right_origin: Option<OpId>,
    },
    Delete {
        target: OpId,
        id: OpId,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sequence<T> {
    elements: Vec<Element<T>>,
    index: BTreeMap<OpId, usize>,
    pending_inserts: BTreeMap<OpId, Vec<SequenceOp<T>>>,
    pending_deletes: BTreeMap<OpId, Vec<SequenceOp<T>>>,
}

impl<T: Clone> Sequence<T> {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            index: BTreeMap::new(),
            pending_inserts: BTreeMap::new(),
            pending_deletes: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, after: Option<OpId>, value: T, id: OpId) {
        let right_origin = self.compute_right_origin(after);
        self.apply(SequenceOp::Insert {
            after,
            id,
            value,
            right_origin,
        });
    }

    pub fn delete(&mut self, target: OpId, id: OpId) {
        self.apply(SequenceOp::Delete { target, id });
    }

    pub fn apply(&mut self, op: SequenceOp<T>) {
        if let Some(inserted_id) = self.apply_now(op) {
            self.process_pending(inserted_id);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.elements.iter().filter_map(|elem| elem.value.as_ref())
    }

    pub fn iter_asc(&self) -> impl Iterator<Item = &T> {
        self.iter()
    }

    pub fn iter_desc(&self) -> impl Iterator<Item = &T> {
        self.elements
            .iter()
            .rev()
            .filter_map(|elem| elem.value.as_ref())
    }

    pub fn iter_all(&self) -> impl Iterator<Item = &Element<T>> {
        self.elements.iter()
    }

    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }

    pub fn len_visible(&self) -> usize {
        self.elements
            .iter()
            .filter(|elem| elem.value.is_some())
            .count()
    }

    pub fn get_element(&self, id: &OpId) -> Option<&Element<T>> {
        self.index.get(id).and_then(|idx| self.elements.get(*idx))
    }

    pub fn update_value(&mut self, id: OpId, value: T) {
        if let Some(index) = self.index.get(&id).copied()
            && let Some(elem) = self.elements.get_mut(index)
        {
            elem.value = Some(value);
        }
    }

    pub fn apply_op(&mut self, op: (OpId, T)) {
        let after = self.elements.last().map(|elem| elem.id);
        self.insert(after, op.1, op.0);
    }

    pub fn from_ordered(items: Vec<(OpId, T)>) -> Self {
        let mut elements = Vec::with_capacity(items.len());
        let mut index = BTreeMap::new();
        let mut after = None;
        for (idx, (id, value)) in items.into_iter().enumerate() {
            elements.push(Element {
                id,
                value: Some(value),
                after,
                right_origin: None,
            });
            index.insert(id, idx);
            after = Some(id);
        }
        Self {
            elements,
            index,
            pending_inserts: BTreeMap::new(),
            pending_deletes: BTreeMap::new(),
        }
    }

    pub fn element_ids(&self) -> Vec<OpId> {
        self.elements.iter().map(|elem| elem.id).collect()
    }

    fn apply_insert(
        &mut self,
        after: Option<OpId>,
        id: &OpId,
        value: &T,
        right_origin: Option<OpId>,
    ) -> bool {
        self.apply_insert_internal(after, id, value, right_origin, true)
    }

    fn apply_insert_internal(
        &mut self,
        after: Option<OpId>,
        id: &OpId,
        value: &T,
        right_origin: Option<OpId>,
        rebuild: bool,
    ) -> bool {
        if self.index.contains_key(id) {
            return true;
        }

        if let Some(anchor) = after
            && !self.index.contains_key(&anchor)
        {
            return false;
        }

        let element = Element {
            id: *id,
            value: Some(value.clone()),
            after,
            right_origin,
        };
        self.elements.push(element);
        if rebuild {
            self.rebuild_order();
        } else {
            let idx = self.elements.len() - 1;
            self.index.insert(*id, idx);
        }
        true
    }

    fn apply_delete(&mut self, target: OpId) -> bool {
        let Some(index) = self.index.get(&target).copied() else {
            return false;
        };
        if let Some(elem) = self.elements.get_mut(index) {
            elem.value = None;
        }
        true
    }

    fn compute_right_origin(&self, after: Option<OpId>) -> Option<OpId> {
        let position = match after {
            None => 0,
            Some(anchor) => self.index.get(&anchor).copied().unwrap_or(0) + 1,
        };
        self.elements.get(position).map(|elem| elem.id)
    }

    fn rebuild_order(&mut self) {
        use std::collections::BTreeMap;

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

        // Use remove() instead of get().cloned() to move elements without cloning
        self.elements = ordered_ids
            .into_iter()
            .filter_map(|id| element_map.remove(&id))
            .collect();
        self.rebuild_index();
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

    fn rebuild_index(&mut self) {
        self.index.clear();
        for (idx, elem) in self.elements.iter().enumerate() {
            self.index.insert(elem.id, idx);
        }
    }

    fn apply_now(&mut self, op: SequenceOp<T>) -> Option<OpId> {
        match op {
            SequenceOp::Insert {
                after,
                id,
                value,
                right_origin,
            } => {
                if self.apply_insert(after, &id, &value, right_origin) {
                    Some(id)
                } else {
                    if let Some(anchor) = after {
                        self.pending_inserts
                            .entry(anchor)
                            .or_default()
                            .push(SequenceOp::Insert {
                                after: Some(anchor),
                                id,
                                value,
                                right_origin,
                            });
                    }
                    None
                }
            }
            SequenceOp::Delete { target, id } => {
                if self.apply_delete(target) {
                    None
                } else {
                    self.pending_deletes
                        .entry(target)
                        .or_default()
                        .push(SequenceOp::Delete { target, id });
                    None
                }
            }
        }
    }

    fn process_pending(&mut self, inserted_id: OpId) {
        use std::collections::VecDeque;
        let mut queue = VecDeque::new();
        self.enqueue_pending(inserted_id, &mut queue);

        let mut inserted = false;
        while let Some(op) = queue.pop_front() {
            match op {
                SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin,
                } => {
                    if self.apply_insert_internal(after, &id, &value, right_origin, false) {
                        inserted = true;
                        self.enqueue_pending(id, &mut queue);
                    } else if let Some(anchor) = after {
                        self.pending_inserts
                            .entry(anchor)
                            .or_default()
                            .push(SequenceOp::Insert {
                                after: Some(anchor),
                                id,
                                value,
                                right_origin,
                            });
                    }
                }
                SequenceOp::Delete { target, id } => {
                    if !self.apply_delete(target) {
                        self.pending_deletes
                            .entry(target)
                            .or_default()
                            .push(SequenceOp::Delete { target, id });
                    }
                }
            }
        }

        if inserted {
            self.rebuild_order();
        }
    }

    fn enqueue_pending(&mut self, id: OpId, queue: &mut std::collections::VecDeque<SequenceOp<T>>) {
        if let Some(ops) = self.pending_inserts.remove(&id) {
            queue.extend(ops);
        }
        if let Some(ops) = self.pending_deletes.remove(&id) {
            queue.extend(ops);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LwwRegister<T> {
    value: T,
    op_id: OpId,
}

impl<T: Clone> LwwRegister<T> {
    pub fn new(value: T, op_id: OpId) -> Self {
        Self { value, op_id }
    }

    pub fn set(&mut self, value: T, op_id: OpId) {
        if op_id >= self.op_id {
            self.value = value;
            self.op_id = op_id;
        }
    }

    /// Returns a clone of the current value. Consider using `get_ref()` to avoid allocation.
    pub fn get(&self) -> T {
        self.value.clone()
    }

    /// Returns a reference to the current value (zero-cost).
    #[inline]
    pub fn get_ref(&self) -> &T {
        &self.value
    }

    pub fn op_id(&self) -> OpId {
        self.op_id
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Map<K, V> {
    entries: BTreeMap<K, LwwRegister<V>>,
}

impl<K: Ord + Clone, V: Clone> Map<K, V> {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, key: K, value: V, op_id: OpId) {
        self.entries
            .entry(key)
            .and_modify(|register| register.set(value.clone(), op_id))
            .or_insert_with(|| LwwRegister::new(value, op_id));
    }

    /// Returns a reference to the value (zero-cost). Use `get_cloned()` if you need ownership.
    #[inline]
    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|register| register.get_ref())
    }

    /// Returns a clone of the value. Prefer `get()` when a reference suffices.
    pub fn get_cloned(&self, key: &K) -> Option<V> {
        self.entries.get(key).map(|register| register.get())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextAnchor {
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkInterval<K, V> {
    pub id: OpId,
    pub start: TextAnchor,
    pub end: TextAnchor,
    pub attributes: BTreeMap<K, LwwRegister<V>>,
}

impl<K: Ord, V: Clone> MarkInterval<K, V> {
    pub fn new(id: OpId, start: TextAnchor, end: TextAnchor) -> Self {
        Self {
            id,
            start,
            end,
            attributes: BTreeMap::new(),
        }
    }

    pub fn update_attribute(&mut self, key: K, value: V, op_id: OpId) {
        self.attributes
            .entry(key)
            .and_modify(|register| register.set(value.clone(), op_id))
            .or_insert_with(|| LwwRegister::new(value, op_id));
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MarkSet<K, V> {
    adds: BTreeMap<OpId, MarkInterval<K, V>>,
    removes: BTreeMap<OpId, OpId>,
}

impl<K: Ord + Clone, V: Clone> MarkSet<K, V> {
    pub fn new() -> Self {
        Self {
            adds: BTreeMap::new(),
            removes: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, interval: MarkInterval<K, V>) {
        self.adds.insert(interval.id, interval);
    }

    pub fn remove(&mut self, add_id: OpId, remove_id: OpId) {
        match self.removes.get(&add_id) {
            Some(existing) if *existing >= remove_id => {}
            _ => {
                self.removes.insert(add_id, remove_id);
            }
        }
    }

    pub fn is_active(&self, add_id: &OpId) -> bool {
        if !self.adds.contains_key(add_id) {
            return false;
        }

        match self.removes.get(add_id) {
            None => true,
            Some(remove_id) => add_id > remove_id,
        }
    }

    pub fn interval(&self, add_id: &OpId) -> Option<&MarkInterval<K, V>> {
        self.adds.get(add_id)
    }
}

pub mod mark {
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
            let entry = self.intervals.entry(interval_id).or_insert(MarkInterval {
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

        pub fn remove_mark(
            &mut self,
            interval_id: MarkIntervalId,
            observed: StateVector,
            op_id: OpId,
        ) {
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
            match self.removes.get(interval_id) {
                None => true,
                Some(remove) => {
                    let seen = remove.observed.get(interval.id.peer).unwrap_or(0);
                    seen < interval.id.counter
                }
            }
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
            for interval in self.active_intervals() {
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
}
