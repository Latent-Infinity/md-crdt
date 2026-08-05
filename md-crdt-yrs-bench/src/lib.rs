//! Controlled competitive benchmarks between `md-crdt` and Yrs.
//!
//! This crate is an unpublished nested workspace. It is excluded from the root
//! workspace so product quality gates never resolve or build Yrs.

pub mod adapter;
pub mod harness;
pub mod md_crdt_adapter;
pub mod report;
pub mod runners;
pub mod scenario;
pub mod sizes;
#[cfg(feature = "sub_probes")]
pub mod sub_probes;
pub mod yrs_adapter;

/// Crate identity for smoke tests and report provenance.
pub const CRATE_NAME: &str = "md-crdt-yrs-bench";

/// Exact Yrs pin required by the comparison methodology.
pub const YRS_VERSION_PIN: &str = "0.27.3";

/// Exact Criterion pin aligned with the product benchmark harness.
pub const CRITERION_VERSION_PIN: &str = "0.5.1";

/// Yrs root shared type name fixed by the comparison methodology.
pub const YRS_TEXT_ROOT: &str = "text";

/// Peer / client identifiers fixed for both engines.
pub const PEER_A: u64 = 1;
pub const PEER_B: u64 = 2;

/// Declared md-crdt wire codec label for Tier C results.
pub const MD_CRDT_WIRE_CODEC: &str = "md_crdt_serde_json_v1";

/// Declared md-crdt binary wire codec label for Tier C′ results (not ratioed with Yrs).
pub const MD_CRDT_BIN_WIRE_CODEC: &str = "md_crdt_bin_v1";

/// Declared Yrs wire codec label for Tier C results.
pub const YRS_WIRE_CODEC: &str = "yrs_lib0_v1";

pub use adapter::{ComparisonAdapter, SyncEngine, TextEngine};
pub use harness::{
    CallTrace, DEFAULT_DESTRUCTIVE_BATCH, DropProbe, TracedInput, run_batched_iteration,
    run_batched_iteration_traced,
};
pub use md_crdt_adapter::{MdCrdtAdapter, MdCrdtBinAdapter, MdCrdtSeed};
pub use report::{
    CaseMetadata, INVOCATION_REQUIRED_KEYS, PROVENANCE_REQUIRED_KEYS, PROVENANCE_SCHEMA_VERSION,
    validate_provenance_document,
};
pub use scenario::{
    BaseHistory, BatchPolicy, EngineId, ScenarioManifest, SequentialExpectation, Tier, Workload,
    all_competitive_manifests, all_stretch_manifests, all_v1_manifests, all_v1_workloads,
    expected_after_middle_insert,
};
pub use sizes::{
    APPEND_LENS, DELTA_LAGS, FANIN_BASE_N, FANIN_PEER_COUNTS, FILL_BYTE, KEYSTROKE_BYTE,
    KEYSTROKE_PAYLOAD, MIDDLE_INSERT, PASTE_LENS, PEER_A_MARKER, PEER_B_MARKER, SizeMatrix,
    TEXT_LENS, V1_SIZE_MATRIX, append_run_payload, fill_text, keystroke_payload, middle_index,
    paste_payload, peer_a, peer_b, peer_marker,
};
pub use yrs_adapter::{YrsAdapter, YrsSeed};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_identity_constants_are_stable() {
        assert_eq!(CRATE_NAME, "md-crdt-yrs-bench");
        assert_eq!(YRS_VERSION_PIN, "0.27.3");
        assert_eq!(CRITERION_VERSION_PIN, "0.5.1");
        assert_eq!(YRS_TEXT_ROOT, "text");
        assert_eq!(PEER_A, 1);
        assert_eq!(PEER_B, 2);
        assert_eq!(MD_CRDT_WIRE_CODEC, "md_crdt_serde_json_v1");
        assert_eq!(MD_CRDT_BIN_WIRE_CODEC, "md_crdt_bin_v1");
        assert_eq!(YRS_WIRE_CODEC, "yrs_lib0_v1");
    }

    #[test]
    fn yrs_dependency_is_linkable() {
        let doc = yrs::Doc::with_client_id(PEER_A);
        assert_eq!(doc.client_id().get(), PEER_A);
    }

    #[test]
    fn md_crdt_dependency_is_linkable() {
        let _session = md_crdt::CollaborativeDocument::new(PEER_A);
    }

    #[test]
    fn v1_manifest_count_is_stable() {
        assert_eq!(all_v1_manifests().len(), 32);
        assert_eq!(all_v1_workloads().len(), 32);
    }
}
