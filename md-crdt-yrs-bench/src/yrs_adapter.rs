//! Yrs adapter over `Doc` + root [`Text`] with frozen methodology options.

use crate::YRS_TEXT_ROOT;
use crate::adapter::{ComparisonAdapter, SyncEngine, TextEngine};
use yrs::updates::decoder::Decode;
use yrs::{
    Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, TextRef, Transact, Update,
};

/// Immutable seed: full lib0 v1 update plus the peer id used to rebuild a fresh `Doc`.
#[derive(Debug, Clone)]
pub struct YrsSeed {
    peer: u64,
    /// Full document state as lib0 v1 update bytes.
    update_v1: Vec<u8>,
}

/// Live Yrs document bound to the fixed root text name.
pub struct YrsAdapter {
    doc: Doc,
    text: TextRef,
}

fn fixed_options(peer: u64) -> Options {
    let mut options = Options::with_client_id(yrs::ClientID::new(peer));
    // Methodology freezes these even when they match defaults.
    options.offset_kind = OffsetKind::Bytes;
    options.skip_gc = false;
    options
}

fn new_doc(peer: u64) -> Doc {
    Doc::with_options(fixed_options(peer))
}

impl YrsAdapter {
    /// Access the underlying document.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Access the root text handle.
    #[must_use]
    pub fn text(&self) -> &TextRef {
        &self.text
    }
}

impl TextEngine for YrsAdapter {
    type Seed = YrsSeed;

    fn seed(peer: u64, text: &str) -> Self::Seed {
        let doc = new_doc(peer);
        let ytext = doc.get_or_insert_text(YRS_TEXT_ROOT);
        {
            let mut txn = doc.transact_mut();
            if !text.is_empty() {
                ytext.insert(&mut txn, 0, text);
            }
        }
        let update_v1 = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        YrsSeed { peer, update_v1 }
    }

    fn restore(seed: &Self::Seed) -> Self {
        let doc = new_doc(seed.peer);
        {
            let mut txn = doc.transact_mut();
            let update = Update::decode_v1(&seed.update_v1).expect("seed Update::decode_v1");
            txn.apply_update(update).expect("seed apply_update");
        }
        let text = doc.get_or_insert_text(YRS_TEXT_ROOT);
        Self { doc, text }
    }

    fn insert_at(&mut self, index: usize, text: &str) {
        let mut txn = self.doc.transact_mut();
        self.text.insert(
            &mut txn,
            u32::try_from(index).expect("index fits u32"),
            text,
        );
    }

    fn delete_at(&mut self, index: usize, len: usize) {
        let mut txn = self.doc.transact_mut();
        self.text.remove_range(
            &mut txn,
            u32::try_from(index).expect("index fits u32"),
            u32::try_from(len).expect("len fits u32"),
        );
    }

    fn visible_len(&self) -> usize {
        let txn = self.doc.transact();
        self.text.len(&txn) as usize
    }

    fn visible_string(&self) -> String {
        let txn = self.doc.transact();
        self.text.get_string(&txn)
    }
}

impl SyncEngine for YrsAdapter {
    type StateVector = StateVector;
    type DecodedUpdate = Update;

    fn state_vector(&self) -> Self::StateVector {
        self.doc.transact().state_vector()
    }

    fn export_decoded_since(&self, sv: &Self::StateVector) -> Self::DecodedUpdate {
        // Yrs exposes updates as lib0 bytes; decode in setup for Tier B.
        let bytes = self.doc.transact().encode_state_as_update_v1(sv);
        Update::decode_v1(&bytes).expect("export_decoded decode_v1")
    }

    fn encode_wire_since(&self, sv: &Self::StateVector) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(sv)
    }

    fn decode_wire(bytes: &[u8]) -> Self::DecodedUpdate {
        Update::decode_v1(bytes).expect("yrs_lib0_v1 decode")
    }

    fn apply_decoded(&mut self, update: Self::DecodedUpdate) {
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).expect("apply_update");
    }
}

impl ComparisonAdapter for YrsAdapter {
    fn empty(peer: u64) -> Self {
        let doc = new_doc(peer);
        let text = doc.get_or_insert_text(YRS_TEXT_ROOT);
        Self { doc, text }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sizes::{MIDDLE_INSERT, fill_text, middle_index};
    use crate::{PEER_A, PEER_B, expected_after_middle_insert};

    #[test]
    fn options_are_frozen() {
        let adapter = YrsAdapter::empty(PEER_A);
        assert_eq!(adapter.doc.offset_kind(), OffsetKind::Bytes);
        assert!(!adapter.doc.skip_gc());
        assert_eq!(adapter.doc.client_id().get(), PEER_A);
    }

    #[test]
    fn middle_insert_exact() {
        let seed = YrsAdapter::seed(PEER_A, &fill_text(10));
        let mut doc = YrsAdapter::restore(&seed);
        doc.insert_at(middle_index(10), MIDDLE_INSERT);
        assert_eq!(doc.visible_string(), expected_after_middle_insert(10));
    }

    #[test]
    fn restore_does_not_share_store() {
        let seed = YrsAdapter::seed(PEER_A, "abcd");
        let mut a = YrsAdapter::restore(&seed);
        let mut b = YrsAdapter::restore(&seed);
        a.insert_at(2, "X");
        assert_eq!(a.visible_string(), "abXcd");
        assert_eq!(b.visible_string(), "abcd");
        b.delete_at(0, 1);
        assert_eq!(b.visible_string(), "bcd");
        assert_eq!(a.visible_string(), "abXcd");
    }

    #[test]
    fn wire_round_trip() {
        let alice = YrsAdapter::restore(&YrsAdapter::seed(PEER_A, "wire"));
        let bytes = alice.encode_wire_since(&StateVector::default());
        let mut bob = YrsAdapter::empty(PEER_B);
        bob.apply_decoded(YrsAdapter::decode_wire(&bytes));
        assert_eq!(bob.visible_string(), "wire");
    }
}
