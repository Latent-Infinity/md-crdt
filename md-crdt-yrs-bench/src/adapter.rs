//! Narrow engine contracts shared by both comparison adapters.
//!
//! Adapters implement these traits in later work. This module only defines the
//! surfaces scenarios and harness code depend on (dependency inversion).

/// Minimal public text document used by controlled scenarios.
///
/// Implementors must rebuild independent documents from [`TextEngine::Seed`]; do
/// not rely on handle cloning for isolation (a clone may alias the same store).
pub trait TextEngine: Sized {
    /// Immutable seed used to rebuild an independent engine instance.
    type Seed: Clone;

    /// Create a seed holding `text` as the sole text-container body for `peer`.
    fn seed(peer: u64, text: &str) -> Self::Seed;

    /// Restore a fresh engine from `seed` (independent of other restores).
    fn restore(seed: &Self::Seed) -> Self;

    /// Insert `text` at `index` (ASCII v1: grapheme == byte == UTF-16 unit).
    fn insert_at(&mut self, index: usize, text: &str);

    /// Delete `len` units starting at `index`.
    fn delete_at(&mut self, index: usize, len: usize);

    /// Visible text-container length (units).
    fn visible_len(&self) -> usize;

    /// Text-container body only (not Markdown serialization).
    fn visible_string(&self) -> String;
}

/// Native state-vector, decoded-integration, and declared wire operations.
pub trait SyncEngine {
    /// Engine-native state vector / clock frontier.
    type StateVector;

    /// Decoded update form used by Tier B integration (and produced by Tier C decode).
    type DecodedUpdate;

    /// Current state vector.
    fn state_vector(&self) -> Self::StateVector;

    /// Tier B prep: engine-native decoded update for everything unknown since `sv`.
    ///
    /// md-crdt may return an in-memory `ChangeMessage`. Yrs may decode lib0 bytes
    /// during setup only — never move that decode into the timed section.
    fn export_decoded_since(&self, sv: &Self::StateVector) -> Self::DecodedUpdate;

    /// Tier C: declared codec bytes for everything unknown since `sv`.
    fn encode_wire_since(&self, sv: &Self::StateVector) -> Vec<u8>;

    /// Tier C: decode declared codec bytes into a native update.
    fn decode_wire(bytes: &[u8]) -> Self::DecodedUpdate;

    /// Integrate a decoded update (Tier B timed work; also used after Tier C decode).
    fn apply_decoded(&mut self, update: Self::DecodedUpdate);
}

/// Full comparison adapter: text + sync, plus an empty receiver constructor.
pub trait ComparisonAdapter: TextEngine + SyncEngine {
    /// Empty document for `peer` (no local content yet). Used to receive remote state.
    fn empty(peer: u64) -> Self;
}

/// Compile-time smoke that generic runners can monomorphize over both traits.
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeSeed {
        peer: u64,
        text: String,
    }

    struct FakeEngine {
        peer: u64,
        text: String,
        clock: u64,
    }

    impl TextEngine for FakeEngine {
        type Seed = FakeSeed;

        fn seed(peer: u64, text: &str) -> Self::Seed {
            FakeSeed {
                peer,
                text: text.to_owned(),
            }
        }

        fn restore(seed: &Self::Seed) -> Self {
            Self {
                peer: seed.peer,
                text: seed.text.clone(),
                clock: 0,
            }
        }

        fn insert_at(&mut self, index: usize, text: &str) {
            self.text.insert_str(index, text);
            self.clock = self.clock.saturating_add(1);
        }

        fn delete_at(&mut self, index: usize, len: usize) {
            let end = index.saturating_add(len).min(self.text.len());
            self.text.replace_range(index..end, "");
            self.clock = self.clock.saturating_add(1);
        }

        fn visible_len(&self) -> usize {
            self.text.len()
        }

        fn visible_string(&self) -> String {
            self.text.clone()
        }
    }

    impl SyncEngine for FakeEngine {
        type StateVector = u64;
        type DecodedUpdate = String;

        fn state_vector(&self) -> Self::StateVector {
            self.clock
        }

        fn export_decoded_since(&self, sv: &Self::StateVector) -> Self::DecodedUpdate {
            format!("decoded:{}:{}:{}", self.peer, sv, self.text)
        }

        fn encode_wire_since(&self, sv: &Self::StateVector) -> Vec<u8> {
            format!("wire:{}:{}:{}", self.peer, sv, self.text).into_bytes()
        }

        fn decode_wire(bytes: &[u8]) -> Self::DecodedUpdate {
            String::from_utf8(bytes.to_vec()).expect("fake wire is utf-8")
        }

        fn apply_decoded(&mut self, update: Self::DecodedUpdate) {
            // Fake integration appends a marker; real adapters merge CRDT updates.
            self.text.push_str(&format!("|{update}|"));
            self.clock = self.clock.saturating_add(1);
        }
    }

    fn middle_insert_once<E: TextEngine>(seed: &E::Seed, index: usize, text: &str) -> String {
        let mut engine = E::restore(seed);
        engine.insert_at(index, text);
        engine.visible_string()
    }

    #[test]
    fn generic_text_runner_monomorphizes() {
        let seed = FakeEngine::seed(1, "xxxx");
        let out = middle_insert_once::<FakeEngine>(&seed, 2, "y");
        assert_eq!(out, "xxyxx");
    }

    #[test]
    fn generic_sync_runner_monomorphizes() {
        let seed = FakeEngine::seed(1, "hi");
        let engine = FakeEngine::restore(&seed);
        let empty = 0u64;
        let decoded = engine.export_decoded_since(&empty);
        assert!(decoded.contains("decoded:"));
        let wire = engine.encode_wire_since(&empty);
        let again = FakeEngine::decode_wire(&wire);
        assert!(again.starts_with("wire:"));
        let mut target = FakeEngine::restore(&seed);
        target.apply_decoded(decoded);
        assert!(target.visible_string().contains("|decoded:"));
    }

    #[test]
    fn seed_restore_is_independent_for_fake_engine() {
        let seed = FakeEngine::seed(1, "ab");
        let mut a = FakeEngine::restore(&seed);
        let mut b = FakeEngine::restore(&seed);
        a.insert_at(1, "X");
        assert_eq!(a.visible_string(), "aXb");
        assert_eq!(b.visible_string(), "ab");
        b.delete_at(0, 1);
        assert_eq!(b.visible_string(), "b");
        assert_eq!(a.visible_string(), "aXb");
    }
}
