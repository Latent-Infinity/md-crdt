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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sequence<T> {
    ops: BTreeMap<OpId, T>,
}

impl<T> Sequence<T> {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
        }
    }

    pub fn apply_op(&mut self, op: (OpId, T)) {
        let (id, value) = op;
        self.ops.entry(id).or_insert(value);
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.iter_desc()
    }

    pub fn iter_desc(&self) -> impl Iterator<Item = &T> + '_ {
        self.ops.values().rev()
    }

    pub fn iter_asc(&self) -> impl Iterator<Item = &T> + '_ {
        self.ops.values()
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

    pub fn get(&self) -> T {
        self.value.clone()
    }

    pub fn op_id(&self) -> OpId {
        self.op_id
    }
}

#[derive(Debug, Default, Clone)]
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

    pub fn get(&self, key: &K) -> Option<V> {
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
