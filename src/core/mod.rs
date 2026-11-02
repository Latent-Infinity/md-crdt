//! Core CRDT algorithms for conflict-free replicated data types.
//!
//! This module provides the fundamental building blocks for CRDT-based
//! collaborative editing:
//!
//! - [`OpId`] - Unique operation identifiers using Lamport timestamps
//! - [`StateVector`] - Version vector for tracking peer state
//! - [`Sequence`] - RGA-based ordered sequence with tombstones
//! - [`LwwRegister`] - Last-writer-wins register for single values
//! - [`Map`] - LWW-based key-value map
//! - [`mark`] - Rich causal mark/formatting CRDT (`MarkSet`, spans)
//! - [`causal`] - Maximal concurrent candidates for causally ordered registers

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod causal;
pub mod mark;

// Unified mark API (rich causal remove-wins). Generic LWW mark types were removed.
pub use mark::{
    Anchor, AnchorBias, MarkInterval, MarkIntervalId, MarkKind, MarkSet, MarkValue, RemoveMark,
    Span,
};

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

    pub fn iter(&self) -> impl Iterator<Item = (PeerId, u64)> + '_ {
        self.peers.iter().map(|(peer, counter)| (*peer, *counter))
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

    /// Whether this build uses sibling-local sequence ordering updates.
    pub const fn incremental_ordering_enabled() -> bool {
        cfg!(feature = "sequence_incremental")
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
        // A ready single insert uses the same splice path as multi-unit chains,
        // avoiding a full rebuild_order for the common case.
        if let Some(insert_at) = self.contiguous_insert_index(std::slice::from_ref(&op)) {
            self.splice_insert_chain(std::iter::once(op), insert_at);
            #[cfg(feature = "sequence_incremental")]
            self.debug_assert_incremental_order();
            return;
        }
        if let Some(inserted_id) = self.apply_now(op) {
            self.process_pending(inserted_id);
        }
        #[cfg(feature = "sequence_incremental")]
        self.debug_assert_incremental_order();
    }

    pub(crate) fn apply_batch(&mut self, ops: impl IntoIterator<Item = SequenceOp<T>>) {
        let ops: Vec<SequenceOp<T>> = ops.into_iter().collect();
        if ops.is_empty() {
            return;
        }
        // Contiguous same-peer insert chains (local paste / multi-grapheme insert)
        // splice once without a full sequence rebuild.
        if let Some(insert_at) = self.contiguous_insert_index(&ops) {
            self.splice_insert_chain(ops, insert_at);
            #[cfg(feature = "sequence_incremental")]
            self.debug_assert_incremental_order();
            return;
        }

        #[cfg(feature = "sequence_incremental")]
        for op in ops {
            self.apply(op);
        }

        #[cfg(not(feature = "sequence_incremental"))]
        {
            let mut inserted = false;
            for op in ops {
                if let Some(inserted_id) = self.apply_now_with_rebuild(op, false) {
                    inserted = true;
                    inserted |= self.process_pending_inner(inserted_id, false);
                }
            }
            if inserted {
                self.rebuild_order();
            }
        }
    }

    /// Fast path for a pure insert chain: each unit after the first anchors on
    /// the previous unit id (local multi-grapheme expand). Places the whole
    /// chain with one sibling-position lookup and one splice; rebuilds only the
    /// id→index map.
    ///
    /// Returns `None` when the batch is not a ready contiguous chain (mixed
    /// deletes, missing anchors, duplicate ids, pending work), so the caller
    /// falls back to the general apply path without consuming any values.
    fn contiguous_insert_index(&self, ops: &[SequenceOp<T>]) -> Option<usize> {
        let first_op = ops.first()?;
        // Refuse when any pending work exists: chain placement assumes a stable
        // linearized order without delayed promotions mid-batch.
        if !self.pending_inserts.is_empty() || !self.pending_deletes.is_empty() {
            return None;
        }

        for (i, op) in ops.iter().enumerate() {
            let SequenceOp::Insert {
                after,
                id,
                value: _,
                right_origin,
            } = op
            else {
                return None;
            };
            if self.index.contains_key(id) {
                return None;
            }
            if i == 0 {
                if let Some(anchor) = after
                    && !self.index.contains_key(anchor)
                {
                    return None;
                }
            } else {
                let SequenceOp::Insert { id: prev_id, .. } = &ops[i - 1] else {
                    return None;
                };
                if *after != Some(*prev_id) {
                    return None;
                }
                // A local expanded insert reserves one consecutive counter run.
                // Enforcing that invariant also prevents duplicate ids within
                // the batch from corrupting the id-to-index map.
                if id.peer != prev_id.peer || prev_id.counter.checked_add(1) != Some(id.counter) {
                    return None;
                }
                // Tail units of a local expand always use right_origin = None.
                if right_origin.is_some() {
                    return None;
                }
            }
        }

        let SequenceOp::Insert {
            after,
            id,
            right_origin,
            ..
        } = first_op
        else {
            return None;
        };
        Some(self.linearized_insert_index(&Element {
            id: *id,
            value: None,
            after: *after,
            right_origin: *right_origin,
        }))
    }

    /// Consume a previously validated insert chain without cloning its values.
    fn splice_insert_chain(
        &mut self,
        ops: impl IntoIterator<Item = SequenceOp<T>>,
        insert_at: usize,
    ) {
        let chain = ops.into_iter().map(|op| match op {
            SequenceOp::Insert {
                after,
                id,
                value,
                right_origin,
            } => Element {
                id,
                value: Some(value),
                after,
                right_origin,
            },
            SequenceOp::Delete { .. } => unreachable!("insert chain was validated before splicing"),
        });
        self.elements.splice(insert_at..insert_at, chain);

        // Shifted suffix indices require O(k log n) BTreeMap updates, in
        // addition to the O(n) sibling scan and contiguous Vec splice.
        for index in insert_at..self.elements.len() {
            self.index.insert(self.elements[index].id, index);
        }
    }

    /// Physical index where `element` belongs among siblings of `element.after`.
    fn linearized_insert_index(&self, element: &Element<T>) -> usize {
        // Match incremental sibling placement: first sibling that sorts after
        // `element` among those with the same `after`, else end of that subtree.
        self.elements
            .iter()
            .enumerate()
            .find(|(_, sibling)| {
                sibling.after == element.after && Self::compare_siblings(element, sibling).is_lt()
            })
            .map(|(index, _)| index)
            .unwrap_or_else(|| self.subtree_end(element.after))
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

    #[cfg(feature = "filesync")]
    pub(crate) fn visible_at_physical(&self, index: usize) -> Option<&T> {
        self.elements.get(index)?.value.as_ref()
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn iter_visible_from_physical(
        &self,
        start: usize,
    ) -> impl Iterator<Item = (usize, &T)> {
        self.elements
            .iter()
            .enumerate()
            .skip(start)
            .filter_map(|(index, element)| element.value.as_ref().map(|value| (index, value)))
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

    /// Transfer a value while retaining its placement as a tombstone anchor.
    pub(crate) fn take_value(&mut self, id: OpId) -> Option<T> {
        let index = *self.index.get(&id)?;
        self.elements.get_mut(index)?.value.take()
    }

    /// Install placement history without manufacturing or copying a live value.
    pub(crate) fn insert_tombstone(
        &mut self,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
    ) -> bool {
        if self.index.contains_key(&id)
            || after.is_some_and(|anchor| !self.index.contains_key(&anchor))
        {
            return false;
        }
        let element = Element {
            id,
            value: None,
            after,
            right_origin,
        };
        self.elements.push(element);
        self.rebuild_order();
        self.process_pending_inner(id, true);
        true
    }

    /// Mutate the value of element `id` in place (no full-element clone).
    ///
    /// Returns `None` if the id is unknown or the element is tombstoned.
    pub fn with_value_mut<R>(&mut self, id: OpId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let index = *self.index.get(&id)?;
        let elem = self.elements.get_mut(index)?;
        let value = elem.value.as_mut()?;
        Some(f(value))
    }

    /// Element ids in storage order (used for tree navigation).
    pub fn ids(&self) -> Vec<OpId> {
        self.elements.iter().map(|e| e.id).collect()
    }

    /// Direct mutable access to a live element's value (`None` if unknown/tombstoned).
    pub fn value_mut(&mut self, id: OpId) -> Option<&mut T> {
        let index = *self.index.get(&id)?;
        self.elements.get_mut(index)?.value.as_mut()
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

    /// Rebuild a sequence from a full element list (including tombstones).
    ///
    /// Used for session snapshot restore. Pending buffers are empty; callers must
    /// ensure `after` / `right_origin` edges are consistent with the stored order.
    pub fn from_elements(elements: Vec<Element<T>>) -> Self {
        let mut index = BTreeMap::new();
        for (idx, elem) in elements.iter().enumerate() {
            index.insert(elem.id, idx);
        }
        Self {
            elements,
            index,
            pending_inserts: BTreeMap::new(),
            pending_deletes: BTreeMap::new(),
        }
    }

    /// Number of retained inserts and deletes awaiting an anchor or target.
    ///
    /// Counts operations, not dependency buckets; duplicate deliveries are
    /// included when they are still pending. Runs in O(dependency buckets).
    pub fn pending_op_count(&self) -> usize {
        self.pending_inserts
            .values()
            .chain(self.pending_deletes.values())
            .map(Vec::len)
            .sum()
    }

    /// Borrow all retained operations without cloning their payloads.
    ///
    /// Callers can account concrete value bytes and promotion work before
    /// admission. This is not a heap-size estimate: collection capacity and
    /// allocator overhead are not represented by iterating occupied entries.
    /// Required operations remain retained until their dependency arrives.
    pub fn pending_ops_iter(&self) -> impl Iterator<Item = &SequenceOp<T>> {
        self.pending_inserts
            .values()
            .chain(self.pending_deletes.values())
            .flatten()
    }

    /// Rebuild materialized elements and their unresolved cross-peer operations.
    pub(crate) fn from_elements_and_pending(
        elements: Vec<Element<T>>,
        pending: Vec<SequenceOp<T>>,
    ) -> Self {
        let mut sequence = Self::from_elements(elements);
        for operation in pending {
            sequence.apply(operation);
        }
        sequence
    }

    pub fn element_ids(&self) -> Vec<OpId> {
        self.elements.iter().map(|elem| elem.id).collect()
    }

    fn apply_insert_internal(
        &mut self,
        after: Option<OpId>,
        id: OpId,
        value: T,
        right_origin: Option<OpId>,
        rebuild: bool,
    ) -> Result<(), T> {
        if self.index.contains_key(&id) {
            return Ok(());
        }

        if let Some(anchor) = after
            && !self.index.contains_key(&anchor)
        {
            return Err(value);
        }

        let element = Element {
            id,
            value: Some(value),
            after,
            right_origin,
        };

        #[cfg(feature = "sequence_incremental")]
        if rebuild {
            self.insert_incrementally(element);
            return Ok(());
        }

        self.elements.push(element);
        if rebuild {
            self.rebuild_order();
        } else {
            let idx = self.elements.len() - 1;
            self.index.insert(id, idx);
        }
        Ok(())
    }

    #[cfg(feature = "sequence_incremental")]
    fn insert_incrementally(&mut self, element: Element<T>) {
        let insert_at = self.linearized_insert_index(&element);

        self.elements.insert(insert_at, element);
        for index in insert_at..self.elements.len() {
            self.index.insert(self.elements[index].id, index);
        }
    }

    fn subtree_end(&self, parent: Option<OpId>) -> usize {
        let Some(parent) = parent else {
            return self.elements.len();
        };
        let parent_index = self.index[&parent];
        // In preorder, the subtree is contiguous. While scanning its prefix,
        // an element is a descendant exactly when its direct anchor is in the
        // prefix already visited (including parent). One O(log n) lookup per
        // element avoids walking its complete ancestry again on deep chains.
        for index in parent_index + 1..self.elements.len() {
            let in_subtree = self.elements[index]
                .after
                .and_then(|anchor| self.index.get(&anchor).copied())
                .is_some_and(|anchor_index| anchor_index >= parent_index && anchor_index < index);
            if !in_subtree {
                return index;
            }
        }
        self.elements.len()
    }

    fn compare_siblings(a: &Element<T>, b: &Element<T>) -> std::cmp::Ordering {
        match (a.right_origin, b.right_origin) {
            (Some(ra), Some(rb)) => {
                if ra == rb {
                    b.id.cmp(&a.id)
                } else {
                    ra.cmp(&rb)
                }
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.id.cmp(&a.id),
        }
    }

    #[cfg(all(feature = "sequence_incremental", debug_assertions))]
    fn debug_assert_incremental_order(&self) {
        crate::perf::timed(crate::perf::Span::SequenceOrderOracle, || {
            let rebuilt_ids = self.reference_order_ids();
            debug_assert!(
                self.elements
                    .iter()
                    .map(|element| element.id)
                    .eq(rebuilt_ids.iter().copied()),
                "incremental sequence order diverged from full rebuild: actual {:?}, rebuilt {:?}",
                self.element_ids(),
                rebuilt_ids,
            );
        });
    }

    #[cfg(all(feature = "sequence_incremental", debug_assertions))]
    fn reference_order_ids(&self) -> Vec<OpId> {
        // Rebuild the entire reference topology from borrowed metadata. Neither
        // the incremental index nor the stored order supplies any parent edges
        // or sibling ordering: both sorts below cover every retained element,
        // including tombstones.
        let mut siblings: Vec<_> = self.elements.iter().collect();
        siblings.sort_unstable_by(|a, b| {
            a.after
                .cmp(&b.after)
                .then_with(|| Self::compare_siblings(a, b))
        });
        let mut by_id: Vec<_> = (0..siblings.len()).collect();
        by_id.sort_unstable_by_key(|&index| siblings[index].id);
        debug_assert!(
            by_id
                .windows(2)
                .all(|pair| siblings[pair[0]].id != siblings[pair[1]].id),
            "full ordering reference contains duplicate identifiers",
        );

        // Parent groups and identifiers are independently sorted, so one merge
        // join links every group's full child range without a temporary map.
        let mut children = vec![0..0; siblings.len()];
        let mut roots = 0..0;
        let mut parent_index = 0;
        let mut start = 0;
        while start < siblings.len() {
            let parent = siblings[start].after;
            let mut end = start + 1;
            while end < siblings.len() && siblings[end].after == parent {
                end += 1;
            }
            if let Some(parent) = parent {
                while parent_index < by_id.len() && siblings[by_id[parent_index]].id < parent {
                    parent_index += 1;
                }
                if parent_index < by_id.len() && siblings[by_id[parent_index]].id == parent {
                    children[by_id[parent_index]] = start..end;
                }
            } else {
                roots = start..end;
            }
            start = end;
        }

        // As in rebuild_order, only rooted nodes are reachable. Missing-parent
        // components or cycles therefore fail the caller's complete ID check;
        // they are not silently appended in their current incremental order.
        let mut stack: Vec<_> = roots.rev().collect();
        let mut ordered_ids = Vec::with_capacity(siblings.len());
        while let Some(index) = stack.pop() {
            ordered_ids.push(siblings[index].id);
            stack.extend(children[index].clone().rev());
        }
        ordered_ids
    }

    #[cfg(all(feature = "sequence_incremental", not(debug_assertions)))]
    fn debug_assert_incremental_order(&self) {}

    fn apply_delete(&mut self, target: OpId) -> bool {
        let Some(index) = self.index.get(&target).copied() else {
            return false;
        };
        if let Some(elem) = self.elements.get_mut(index) {
            elem.value = None;
        }
        true
    }

    /// Right-neighbor id used for RGA concurrent-insert ordering at `after`.
    ///
    /// Exposed so the session layer can stamp wire `right_origin` (N4) without
    /// reimplementing sequence layout.
    pub fn compute_right_origin(&self, after: Option<OpId>) -> Option<OpId> {
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
                let elem_a = element_map
                    .get(a)
                    .expect("child id must exist in element map during rebuild");
                let elem_b = element_map
                    .get(b)
                    .expect("child id must exist in element map during rebuild");
                Self::compare_siblings(elem_a, elem_b)
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
        let mut stack = children
            .get(&parent)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .collect::<Vec<_>>();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(kids) = children.get(&Some(id)) {
                stack.extend(kids.iter().rev().copied());
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
        self.apply_now_with_rebuild(op, true)
    }

    fn apply_now_with_rebuild(&mut self, op: SequenceOp<T>, rebuild: bool) -> Option<OpId> {
        match op {
            SequenceOp::Insert {
                after,
                id,
                value,
                right_origin,
            } => {
                if let Err(value) =
                    self.apply_insert_internal(after, id, value, right_origin, rebuild)
                {
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
                } else {
                    Some(id)
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
        let inserted =
            self.process_pending_inner(inserted_id, cfg!(feature = "sequence_incremental"));
        if inserted && !cfg!(feature = "sequence_incremental") {
            self.rebuild_order();
        }
    }

    fn process_pending_inner(&mut self, inserted_id: OpId, rebuild: bool) -> bool {
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
                    if let Err(value) =
                        self.apply_insert_internal(after, id, value, right_origin, rebuild)
                    {
                        if let Some(anchor) = after {
                            self.pending_inserts.entry(anchor).or_default().push(
                                SequenceOp::Insert {
                                    after: Some(anchor),
                                    id,
                                    value,
                                    right_origin,
                                },
                            );
                        }
                    } else {
                        inserted = true;
                        self.enqueue_pending(id, &mut queue);
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

        inserted
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

#[cfg(test)]
mod batch_tests {
    use super::{OpId, Sequence, SequenceOp};

    fn id(counter: u64) -> OpId {
        OpId { counter, peer: 1 }
    }

    fn insert(after: Option<OpId>, counter: u64, value: char) -> SequenceOp<char> {
        SequenceOp::Insert {
            after,
            id: id(counter),
            value,
            right_origin: None,
        }
    }

    fn apply_with_full_rebuild(sequence: &mut Sequence<char>, ops: Vec<SequenceOp<char>>) {
        let mut inserted = false;
        for op in ops {
            if let Some(inserted_id) = sequence.apply_now_with_rebuild(op, false) {
                inserted = true;
                inserted |= sequence.process_pending_inner(inserted_id, false);
            }
        }
        if inserted {
            sequence.rebuild_order();
        }
    }

    #[test]
    fn batched_apply_matches_individual_apply_and_promotes_pending_ops() {
        let pending = insert(Some(id(3)), 4, 'd');
        let chain = vec![
            insert(None, 1, 'a'),
            insert(Some(id(1)), 2, 'b'),
            insert(Some(id(2)), 3, 'c'),
        ];

        let mut individual = Sequence::new();
        individual.apply(pending.clone());
        for op in chain.clone() {
            individual.apply(op);
        }

        let mut batched = Sequence::new();
        batched.apply(pending);
        batched.apply_batch(chain);

        assert_eq!(batched, individual);
        assert_eq!(batched.iter().copied().collect::<String>(), "abcd");
    }

    #[test]
    fn contiguous_chain_middle_insert_matches_full_rebuild() {
        let mut base = Sequence::new();
        for (i, ch) in ['w', 'x', 'y', 'z'].into_iter().enumerate() {
            base.apply(insert(
                if i == 0 { None } else { Some(id(i as u64)) },
                (i + 1) as u64,
                ch,
            ));
        }
        // Insert "ab" after 'x' (id 2): chain after=2, then after first new.
        let chain = vec![
            SequenceOp::Insert {
                after: Some(id(2)),
                id: id(10),
                value: 'a',
                right_origin: base.compute_right_origin(Some(id(2))),
            },
            SequenceOp::Insert {
                after: Some(id(10)),
                id: id(11),
                value: 'b',
                right_origin: None,
            },
        ];

        let mut rebuilt = base.clone();
        apply_with_full_rebuild(&mut rebuilt, chain.clone());

        let mut batched = base;
        batched.apply_batch(chain);

        assert_eq!(batched, rebuilt);
        assert_eq!(batched.iter().copied().collect::<String>(), "wxabyz");
        assert_eq!(batched.element_ids(), rebuilt.element_ids());
    }

    #[test]
    fn duplicate_id_chain_falls_back_without_corrupting_the_index() {
        let mut base = Sequence::new();
        base.apply(insert(None, 1, 'a'));
        let duplicate = vec![
            insert(Some(id(1)), 10, 'x'),
            SequenceOp::Insert {
                after: Some(id(10)),
                id: id(10),
                value: 'y',
                right_origin: None,
            },
        ];

        let mut rebuilt = base.clone();
        apply_with_full_rebuild(&mut rebuilt, duplicate.clone());
        let mut batched = base;
        batched.apply_batch(duplicate);

        assert_eq!(batched, rebuilt);
        assert_eq!(
            batched
                .element_ids()
                .into_iter()
                .filter(|candidate| *candidate == id(10))
                .count(),
            1
        );
    }

    #[test]
    fn non_chain_batch_still_converges() {
        // Two inserts with the same after (siblings) are not a parent chain.
        let ops = vec![
            SequenceOp::Insert {
                after: None,
                id: id(1),
                value: 'a',
                right_origin: None,
            },
            SequenceOp::Insert {
                after: None,
                id: id(2),
                value: 'b',
                right_origin: None,
            },
        ];
        let mut individual = Sequence::new();
        for op in ops.clone() {
            individual.apply(op);
        }
        let mut batched = Sequence::new();
        batched.apply_batch(ops);
        assert_eq!(batched, individual);
    }

    #[test]
    fn owned_insert_values_keep_their_allocations_across_every_apply_path() {
        fn owned_insert(
            after: Option<OpId>,
            counter: u64,
            text: &str,
        ) -> (SequenceOp<String>, *const u8) {
            let value = text.to_owned();
            let allocation = value.as_ptr();
            (
                SequenceOp::Insert {
                    after,
                    id: id(counter),
                    value,
                    right_origin: None,
                },
                allocation,
            )
        }

        fn allocation(sequence: &Sequence<String>, counter: u64) -> *const u8 {
            sequence
                .get_element(&id(counter))
                .and_then(|element| element.value.as_ref())
                .expect("inserted value must be live")
                .as_ptr()
        }

        let mut sequence = Sequence::new();
        let (root, root_allocation) = owned_insert(None, 1, "root");
        sequence.apply(root);
        assert_eq!(allocation(&sequence, 1), root_allocation);

        let (first, first_allocation) = owned_insert(Some(id(1)), 2, "first");
        let (second, second_allocation) = owned_insert(Some(id(2)), 3, "second");
        sequence.apply_batch([first, second]);
        assert_eq!(allocation(&sequence, 2), first_allocation);
        assert_eq!(allocation(&sequence, 3), second_allocation);

        let (pending, pending_allocation) = owned_insert(Some(id(4)), 5, "pending");
        sequence.apply(pending);
        assert_eq!(sequence.pending_op_count(), 1);
        let (anchor, anchor_allocation) = owned_insert(Some(id(3)), 4, "anchor");
        sequence.apply(anchor);
        assert_eq!(allocation(&sequence, 4), anchor_allocation);
        assert_eq!(allocation(&sequence, 5), pending_allocation);
        assert_eq!(sequence.pending_op_count(), 0);

        // A mixed batch exercises the general owned-value path rather than the
        // contiguous-chain specialization.
        let (last, last_allocation) = owned_insert(Some(id(5)), 6, "last");
        sequence.apply_batch([
            last,
            SequenceOp::Delete {
                target: id(1),
                id: id(7),
            },
        ]);
        assert_eq!(allocation(&sequence, 6), last_allocation);
        assert_eq!(
            sequence.iter().map(String::as_str).collect::<Vec<_>>(),
            ["first", "second", "anchor", "pending", "last"]
        );
    }
}

#[cfg(all(test, feature = "sequence_incremental", debug_assertions))]
mod ordering_reference_tests {
    use super::{Element, OpId, Sequence};

    fn id(counter: u64, peer: u64) -> OpId {
        OpId { counter, peer }
    }

    fn element(
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        live: bool,
    ) -> Element<u8> {
        Element {
            id,
            after,
            right_origin,
            value: live.then_some(1),
        }
    }

    fn metadata(elements: &[Element<u8>]) -> Vec<Element<()>> {
        elements
            .iter()
            .map(|element| Element {
                id: element.id,
                value: None,
                after: element.after,
                right_origin: element.right_origin,
            })
            .collect()
    }

    fn no_initial_index_rebuild_ids(elements: &[Element<u8>]) -> Vec<OpId> {
        // Attribution precursor only: the full legacy rebuild replaces this
        // empty index without reading it. The shipped borrowed oracle supersedes
        // this smaller change rather than accumulating it as a second feature.
        let mut sequence = Sequence {
            elements: metadata(elements),
            index: Default::default(),
            pending_inserts: Default::default(),
            pending_deletes: Default::default(),
        };
        sequence.rebuild_order();
        sequence.element_ids()
    }

    fn full_rebuild_ids(elements: &[Element<u8>]) -> Vec<OpId> {
        let mut sequence = Sequence::from_elements(metadata(elements));
        sequence.rebuild_order();
        let ids = sequence.element_ids();
        assert_eq!(ids, no_initial_index_rebuild_ids(elements));
        ids
    }

    fn assert_permutations(elements: &mut [Element<u8>], offset: usize, expected: &[OpId]) {
        if offset == elements.len() {
            let mut sequence = Sequence::from_elements(elements.to_vec());
            // The reference must not trust even a populated incremental index.
            sequence.index.clear();
            assert_eq!(sequence.reference_order_ids(), expected);
            return;
        }
        for index in offset..elements.len() {
            elements.swap(offset, index);
            assert_permutations(elements, offset + 1, expected);
            elements.swap(offset, index);
        }
    }

    #[test]
    fn borrowed_order_reference_matches_full_rebuild_for_every_metadata_permutation() {
        let root = id(1, 1);
        let first = id(2, 1);
        let tie = id(2, 2);
        let earlier_origin = id(4, 3);
        let unbounded = id(5, 1);
        let descendant = id(6, 2);
        let mut elements = vec![
            element(root, None, None, true),
            element(id(7, 3), None, None, false),
            element(first, Some(root), Some(descendant), true),
            element(tie, Some(root), Some(descendant), false),
            element(earlier_origin, Some(root), Some(unbounded), true),
            element(unbounded, Some(root), None, true),
            element(descendant, Some(first), None, false),
        ];
        let expected = full_rebuild_ids(&elements);
        assert_eq!(expected.len(), elements.len());
        assert_permutations(&mut elements, 0, &expected);
    }

    #[test]
    fn borrowed_order_reference_matches_deep_and_wide_full_rebuilds() {
        for chain in [false, true] {
            let elements: Vec<_> = (1..=4096)
                .map(|counter| {
                    let after = if counter == 1 {
                        None
                    } else if chain {
                        Some(id(counter - 1, 1))
                    } else {
                        Some(id(1, 1))
                    };
                    let right_origin = (counter % 3 == 0).then_some(id(2, 1));
                    element(id(counter, 1), after, right_origin, counter % 2 == 0)
                })
                .collect();
            let expected = full_rebuild_ids(&elements);
            let mut reversed = elements;
            reversed.reverse();
            let sequence = Sequence::from_elements(reversed);
            assert_eq!(sequence.reference_order_ids(), expected);
        }
    }

    #[test]
    fn borrowed_order_reference_preserves_unrooted_component_rejection() {
        for unrooted in [
            vec![element(id(2, 1), Some(id(99, 1)), None, false)],
            vec![
                element(id(2, 1), Some(id(3, 1)), None, true),
                element(id(3, 1), Some(id(2, 1)), None, false),
            ],
        ] {
            let mut elements = vec![element(id(1, 1), None, None, true)];
            elements.extend(unrooted);
            let expected = full_rebuild_ids(&elements);
            let sequence = Sequence::from_elements(elements);
            assert_eq!(sequence.reference_order_ids(), expected);
            assert!(
                std::panic::catch_unwind(|| sequence.debug_assert_incremental_order()).is_err()
            );
        }
    }

    #[test]
    #[should_panic(expected = "incremental sequence order diverged from full rebuild")]
    fn borrowed_order_reference_detects_wrong_stored_order() {
        let sequence = Sequence::from_elements(vec![
            element(id(2, 1), Some(id(1, 1)), None, true),
            element(id(1, 1), None, None, false),
        ]);
        sequence.debug_assert_incremental_order();
    }

    #[test]
    #[should_panic(expected = "full ordering reference contains duplicate identifiers")]
    fn borrowed_order_reference_rejects_duplicate_retained_identifiers() {
        let sequence = Sequence::from_elements(vec![
            element(id(1, 1), None, None, true),
            element(id(1, 1), Some(id(1, 1)), None, false),
        ]);
        sequence.debug_assert_incremental_order();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
