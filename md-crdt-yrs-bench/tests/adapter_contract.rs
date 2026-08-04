//! Parameterized contract matrix over both comparison engines.

use md_crdt_yrs_bench::scenario::{
    BaseHistory, SequentialExpectation, all_v1_manifests, expected_after_middle_insert,
};
use md_crdt_yrs_bench::sizes::{
    MIDDLE_INSERT, append_run_payload, fill_text, keystroke_payload, middle_index,
};
use md_crdt_yrs_bench::{ComparisonAdapter, MdCrdtAdapter, PEER_A, PEER_B, TextEngine, YrsAdapter};

fn middle_insert_exact<E: TextEngine>() {
    for n in [8usize, 1_000, 10_000] {
        let seed = E::seed(PEER_A, &fill_text(n));
        let mut doc = E::restore(&seed);
        doc.insert_at(middle_index(n), MIDDLE_INSERT);
        assert_eq!(doc.visible_string(), expected_after_middle_insert(n));
        assert_eq!(doc.visible_len(), n + 1);
    }
}

fn seed_restore_isolation<E: TextEngine>() {
    let seed = E::seed(PEER_A, "abcd");
    let mut a = E::restore(&seed);
    let mut b = E::restore(&seed);
    a.insert_at(2, "X");
    assert_eq!(a.visible_string(), "abXcd");
    assert_eq!(b.visible_string(), "abcd");
    b.delete_at(0, 1);
    assert_eq!(b.visible_string(), "bcd");
    assert_eq!(a.visible_string(), "abXcd");
    let c = E::restore(&seed);
    assert_eq!(c.visible_string(), "abcd");
}

fn append_and_delete_paths<E: TextEngine>() {
    let seed = E::seed(PEER_A, "xx");
    let mut doc = E::restore(&seed);
    doc.insert_at(2, "zz");
    assert_eq!(doc.visible_string(), "xxzz");
    doc.delete_at(1, 2);
    assert_eq!(doc.visible_string(), "xz");
}

fn two_peer_sync_exact<E: ComparisonAdapter>()
where
    E::StateVector: Default,
{
    let alice_seed = E::seed(PEER_A, "hello");
    let alice = E::restore(&alice_seed);
    let empty = E::StateVector::default();
    let full = alice.export_decoded_since(&empty);

    let mut bob = E::empty(PEER_B);
    bob.apply_decoded(full);
    assert_eq!(bob.visible_string(), "hello");

    let mut alice = E::restore(&alice_seed);
    let bob_sv = bob.state_vector();
    alice.insert_at(5, "!");
    let delta = alice.export_decoded_since(&bob_sv);
    bob.apply_decoded(delta);
    assert_eq!(alice.visible_string(), "hello!");
    assert_eq!(bob.visible_string(), "hello!");
}

fn wire_round_trip_apply<E: ComparisonAdapter>()
where
    E::StateVector: Default,
{
    let alice = E::restore(&E::seed(PEER_A, "wire"));
    let bytes = alice.encode_wire_since(&E::StateVector::default());
    assert!(!bytes.is_empty());
    let mut bob = E::empty(PEER_B);
    bob.apply_decoded(E::decode_wire(&bytes));
    assert_eq!(bob.visible_string(), "wire");
}

fn all_public_text_manifests<E: TextEngine>() {
    for manifest in all_v1_manifests()
        .into_iter()
        .filter(|m| m.workload_id.starts_with("text_"))
    {
        let n = match manifest.base_history {
            BaseHistory::BulkSeed { n } => n,
            BaseHistory::BulkSeedPlusLag { n, .. } => n,
        };
        let seed = E::seed(manifest.peer_a, &fill_text(n));
        let mut doc = E::restore(&seed);
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

fn append_run_spot_check<E: TextEngine>() {
    let seed = E::seed(PEER_A, &fill_text(4));
    let mut doc = E::restore(&seed);
    doc.insert_at(4, &append_run_payload(3));
    assert_eq!(doc.visible_string(), "xxxxzzz");
}

// --- md-crdt ---

#[test]
fn md_crdt_middle_insert_exact() {
    middle_insert_exact::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_seed_restore_isolation() {
    seed_restore_isolation::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_append_and_delete_paths() {
    append_and_delete_paths::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_two_peer_sync_exact() {
    two_peer_sync_exact::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_wire_round_trip_apply() {
    wire_round_trip_apply::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_all_public_text_manifests() {
    all_public_text_manifests::<MdCrdtAdapter>();
}

#[test]
fn md_crdt_append_run_spot_check() {
    append_run_spot_check::<MdCrdtAdapter>();
}

// --- Yrs ---

#[test]
fn yrs_middle_insert_exact() {
    middle_insert_exact::<YrsAdapter>();
}

#[test]
fn yrs_seed_restore_isolation() {
    seed_restore_isolation::<YrsAdapter>();
}

#[test]
fn yrs_append_and_delete_paths() {
    append_and_delete_paths::<YrsAdapter>();
}

#[test]
fn yrs_two_peer_sync_exact() {
    two_peer_sync_exact::<YrsAdapter>();
}

#[test]
fn yrs_wire_round_trip_apply() {
    wire_round_trip_apply::<YrsAdapter>();
}

#[test]
fn yrs_all_public_text_manifests() {
    all_public_text_manifests::<YrsAdapter>();
}

#[test]
fn yrs_append_run_spot_check() {
    append_run_spot_check::<YrsAdapter>();
}
