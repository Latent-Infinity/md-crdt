//! md-crdt adapter over public [`CollaborativeDocument`] APIs.

use crate::adapter::{ComparisonAdapter, SyncEngine, TextEngine};
use md_crdt::doc::{block_text_seq, paragraph_visible_string};
use md_crdt::{
    BlockId, ChangeMessage, CollaborativeDocument, SessionSnapshot, StateVector, ValidationLimits,
    block_id_from_op,
};

/// Immutable seed: full session snapshot plus the paragraph block identity.
#[derive(Debug, Clone)]
pub struct MdCrdtSeed {
    snapshot: SessionSnapshot,
    block_id: BlockId,
}

/// Live collaborative session bound to a single text-container paragraph.
pub struct MdCrdtAdapter {
    session: CollaborativeDocument,
    block_id: BlockId,
}

impl MdCrdtAdapter {
    /// Access the underlying session (sync diagnostics / benches).
    #[must_use]
    pub fn session(&self) -> &CollaborativeDocument {
        &self.session
    }

    /// Paragraph [`BlockId`] used by text operations.
    #[must_use]
    pub fn block_id(&self) -> BlockId {
        self.block_id
    }

    fn refresh_block_id(&mut self) {
        if let Some(block) = self.session.document().blocks_in_order().first() {
            self.block_id = block.id;
        }
    }

    fn paragraph_body(&self) -> String {
        let Some(block) = self.session.document().find_block_by_id(self.block_id) else {
            // Empty receiver before first apply, or missing block.
            return String::new();
        };
        let Some(seq) = block_text_seq(&block.kind) else {
            return String::new();
        };
        paragraph_visible_string(seq)
    }
}

impl ComparisonAdapter for MdCrdtAdapter {
    fn empty(peer: u64) -> Self {
        Self {
            session: CollaborativeDocument::new(peer),
            // Placeholder until the first block arrives via apply or local seed.
            block_id: block_id_from_op(md_crdt::OpId {
                counter: 0,
                peer: 0,
            }),
        }
    }
}

impl TextEngine for MdCrdtAdapter {
    type Seed = MdCrdtSeed;

    fn seed(peer: u64, text: &str) -> Self::Seed {
        let mut session = CollaborativeDocument::new(peer);
        let elem = session
            .insert_paragraph(None, text)
            .expect("seed insert_paragraph");
        let block_id = block_id_from_op(elem);
        let snapshot = session.save_snapshot().expect("seed save_snapshot");
        MdCrdtSeed { snapshot, block_id }
    }

    fn restore(seed: &Self::Seed) -> Self {
        let session =
            CollaborativeDocument::restore_from_snapshot(seed.snapshot.clone()).expect("restore");
        Self {
            session,
            block_id: seed.block_id,
        }
    }

    fn insert_at(&mut self, index: usize, text: &str) {
        self.session
            .insert_text(self.block_id, index, text)
            .expect("insert_text");
    }

    fn delete_at(&mut self, index: usize, len: usize) {
        self.session
            .delete_text(self.block_id, index, len)
            .expect("delete_text");
    }

    fn visible_len(&self) -> usize {
        let Some(block) = self.session.document().find_block_by_id(self.block_id) else {
            return 0;
        };
        block_text_seq(&block.kind)
            .map(md_crdt::Sequence::len_visible)
            .unwrap_or(0)
    }

    fn visible_string(&self) -> String {
        self.paragraph_body()
    }
}

impl SyncEngine for MdCrdtAdapter {
    type StateVector = StateVector;
    type DecodedUpdate = ChangeMessage;

    fn state_vector(&self) -> Self::StateVector {
        self.session.state_vector()
    }

    fn export_decoded_since(&self, sv: &Self::StateVector) -> Self::DecodedUpdate {
        self.session
            .encode_changes_since(sv)
            .expect("encode_changes_since")
    }

    fn encode_wire_since(&self, sv: &Self::StateVector) -> Vec<u8> {
        let message = self.export_decoded_since(sv);
        serde_json::to_vec(&message).expect("md_crdt_serde_json_v1 encode")
    }

    fn decode_wire(bytes: &[u8]) -> Self::DecodedUpdate {
        serde_json::from_slice(bytes).expect("md_crdt_serde_json_v1 decode")
    }

    fn apply_decoded(&mut self, update: Self::DecodedUpdate) {
        self.session
            .apply_remote(update, &ValidationLimits::default())
            .expect("apply_remote");
        self.refresh_block_id();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{
        SequentialExpectation, Workload, all_v1_manifests, expected_after_middle_insert,
    };
    use crate::sizes::{
        MIDDLE_INSERT, append_run_payload, fill_text, keystroke_payload, middle_index,
    };
    use crate::{PEER_A, PEER_B, TextEngine};

    #[test]
    fn middle_insert_matches_exact_expectation() {
        for n in [8usize, 1_000, 10_000] {
            let seed = MdCrdtAdapter::seed(PEER_A, &fill_text(n));
            let mut doc = MdCrdtAdapter::restore(&seed);
            doc.insert_at(middle_index(n), MIDDLE_INSERT);
            assert_eq!(doc.visible_string(), expected_after_middle_insert(n));
            assert_eq!(doc.visible_len(), n + 1);
        }
    }

    #[test]
    fn seed_restore_isolation() {
        let seed = MdCrdtAdapter::seed(PEER_A, "abcd");
        let mut a = MdCrdtAdapter::restore(&seed);
        let mut b = MdCrdtAdapter::restore(&seed);
        a.insert_at(2, "X");
        assert_eq!(a.visible_string(), "abXcd");
        assert_eq!(b.visible_string(), "abcd");
        b.delete_at(0, 1);
        assert_eq!(b.visible_string(), "bcd");
        assert_eq!(a.visible_string(), "abXcd");
        // Seed remains usable.
        let c = MdCrdtAdapter::restore(&seed);
        assert_eq!(c.visible_string(), "abcd");
    }

    #[test]
    fn append_and_delete_paths() {
        let seed = MdCrdtAdapter::seed(PEER_A, "xx");
        let mut doc = MdCrdtAdapter::restore(&seed);
        doc.insert_at(2, "zz");
        assert_eq!(doc.visible_string(), "xxzz");
        doc.delete_at(1, 2);
        assert_eq!(doc.visible_string(), "xz");
    }

    #[test]
    fn export_decoded_is_change_message_not_raw_only_bytes() {
        let seed = MdCrdtAdapter::seed(PEER_A, "hi");
        let doc = MdCrdtAdapter::restore(&seed);
        let empty = StateVector::default();
        let message: ChangeMessage = doc.export_decoded_since(&empty);
        assert!(!message.ops.is_empty());
        // Wire path is an additional JSON container over the same selection.
        let wire = doc.encode_wire_since(&empty);
        let decoded = MdCrdtAdapter::decode_wire(&wire);
        assert_eq!(decoded.ops.len(), message.ops.len());
        assert_eq!(decoded.since, message.since);
    }

    #[test]
    fn two_peer_sync_exact_content_and_convergence() {
        let alice_seed = MdCrdtAdapter::seed(PEER_A, "hello");
        let alice = MdCrdtAdapter::restore(&alice_seed);
        let empty = StateVector::default();
        let full = alice.export_decoded_since(&empty);

        let mut bob = MdCrdtAdapter::empty(PEER_B);
        bob.apply_decoded(full);
        assert_eq!(bob.visible_string(), "hello");
        assert_eq!(bob.block_id(), alice.block_id());

        // Concurrent-style sequential follow-up: Alice appends, Bob applies delta.
        let mut alice = MdCrdtAdapter::restore(&alice_seed);
        // Re-sync bob from alice base again via seed of bob's current state.
        let bob_sv = bob.state_vector();
        alice.insert_at(5, "!");
        let delta = alice.export_decoded_since(&bob_sv);
        bob.apply_decoded(delta);
        assert_eq!(alice.visible_string(), "hello!");
        assert_eq!(bob.visible_string(), "hello!");
    }

    #[test]
    fn wire_round_trip_apply() {
        let alice_seed = MdCrdtAdapter::seed(PEER_A, "wire");
        let alice = MdCrdtAdapter::restore(&alice_seed);
        let bytes = alice.encode_wire_since(&StateVector::default());
        assert!(!bytes.is_empty());
        let mut bob = MdCrdtAdapter::empty(PEER_B);
        bob.apply_decoded(MdCrdtAdapter::decode_wire(&bytes));
        assert_eq!(bob.visible_string(), "wire");
    }

    #[test]
    fn all_public_text_manifests_match_expectations() {
        for manifest in all_v1_manifests()
            .into_iter()
            .filter(|m| m.workload_id.starts_with("text_"))
        {
            let n = match manifest.base_history {
                crate::scenario::BaseHistory::BulkSeed { n } => n,
                crate::scenario::BaseHistory::BulkSeedPlusLag { n, .. } => n,
            };
            let seed = MdCrdtAdapter::seed(manifest.peer_a, &fill_text(n));
            let mut doc = MdCrdtAdapter::restore(&seed);
            match manifest.workload_id {
                "text_insert_middle" => {
                    doc.insert_at(
                        manifest.edit_index.expect("index"),
                        manifest.edit_payload.as_deref().expect("payload"),
                    );
                }
                "text_append_run" => {
                    let payload = manifest.edit_payload.as_deref().expect("payload");
                    doc.insert_at(n, payload);
                }
                "text_append_keystrokes" => {
                    let unit = keystroke_payload();
                    for i in 0..manifest.timed_api_calls {
                        doc.insert_at(n + i, unit);
                    }
                }
                "text_delete_middle" => {
                    doc.delete_at(manifest.edit_index.expect("index"), 1);
                }
                other => panic!("unexpected text workload {other}"),
            }
            match &manifest.expectation {
                SequentialExpectation::ExactVisible(expected) => {
                    assert_eq!(
                        doc.visible_string(),
                        *expected,
                        "workload {} {}",
                        manifest.workload_id,
                        manifest.parameter_id
                    );
                }
                SequentialExpectation::ConcurrentMarkers { .. }
                | SequentialExpectation::FanInMarkers { .. } => {
                    panic!("text workloads are sequential")
                }
            }
        }
    }

    #[test]
    fn workload_helpers_align_with_matrix() {
        // Spot-check pure expected strings used above still match adapters.
        let run = Workload::TextAppendRun { n: 4, m: 3 }.to_manifest();
        let seed = MdCrdtAdapter::seed(PEER_A, &fill_text(4));
        let mut doc = MdCrdtAdapter::restore(&seed);
        doc.insert_at(4, &append_run_payload(3));
        match run.expectation {
            SequentialExpectation::ExactVisible(e) => assert_eq!(doc.visible_string(), e),
            SequentialExpectation::ConcurrentMarkers { .. }
            | SequentialExpectation::FanInMarkers { .. } => unreachable!(),
        }
    }
}
