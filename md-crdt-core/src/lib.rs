use std::collections::BTreeMap;

pub type PeerId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId {
    pub counter: u64,
    pub peer: PeerId,
}

pub struct StateVector {
    peers: BTreeMap<PeerId, u64>,
}

impl Default for StateVector {
    fn default() -> Self {
        Self::new()
    }
}

impl StateVector {
    pub fn new() -> Self {
        StateVector {
            peers: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}
