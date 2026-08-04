//! Setup/measure runners for competitive workloads (shared by tests and Criterion).

use crate::adapter::ComparisonAdapter;
use crate::scenario::{BaseHistory, ScenarioManifest};
use crate::sizes::{
    PEER_A_MARKER, PEER_B_MARKER, fill_text, keystroke_payload, middle_index, peer_marker,
};

/// Apply K one-byte lag keystrokes after a bulk seed of length `n` (frozen history).
pub fn apply_lag_keystrokes<E: ComparisonAdapter>(doc: &mut E, n: usize, k: usize) {
    for i in 0..k {
        doc.insert_at(n.saturating_add(i), keystroke_payload());
    }
}

/// Build a live document matching `base_history` for `peer`.
pub fn build_from_history<E: ComparisonAdapter>(peer: u64, history: BaseHistory) -> E {
    match history {
        BaseHistory::BulkSeed { n } => E::restore(&E::seed(peer, &fill_text(n))),
        BaseHistory::BulkSeedPlusLag { n, k } => {
            let mut doc = E::restore(&E::seed(peer, &fill_text(n)));
            apply_lag_keystrokes(&mut doc, n, k);
            doc
        }
    }
}

fn base_n(history: BaseHistory) -> usize {
    match history {
        BaseHistory::BulkSeed { n } | BaseHistory::BulkSeedPlusLag { n, .. } => n,
    }
}

fn lag_k(history: BaseHistory) -> Option<usize> {
    match history {
        BaseHistory::BulkSeed { .. } => None,
        BaseHistory::BulkSeedPlusLag { k, .. } => Some(k),
    }
}

/// Run the timed body of a Tier A public-text workload (for contract checks).
pub fn run_public_text_once<E: ComparisonAdapter>(manifest: &ScenarioManifest) -> String {
    let n = base_n(manifest.base_history);
    let mut doc = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
    match manifest.workload_id {
        "text_insert_middle" => {
            doc.insert_at(
                manifest.edit_index.expect("index"),
                manifest.edit_payload.as_deref().expect("payload"),
            );
        }
        "text_append_run" => {
            doc.insert_at(n, manifest.edit_payload.as_deref().expect("payload"));
        }
        "text_append_keystrokes" => {
            let unit = keystroke_payload();
            for i in 0..manifest.timed_api_calls {
                doc.insert_at(n.saturating_add(i), unit);
            }
        }
        "text_delete_middle" => {
            doc.delete_at(manifest.edit_index.expect("index"), 1);
        }
        "text_paste_middle" => {
            doc.insert_at(
                manifest.edit_index.expect("index"),
                manifest.edit_payload.as_deref().expect("payload"),
            );
        }
        other => panic!("not a public text workload: {other}"),
    }
    doc.visible_string()
}

/// Sample wire payload bytes for a Tier C case (setup only).
pub fn sample_wire_payload_bytes<E: ComparisonAdapter>(manifest: &ScenarioManifest) -> usize
where
    E::StateVector: Default + Clone,
{
    match manifest.workload_id {
        "wire_encode_full" => {
            let doc = build_from_history::<E>(manifest.peer_a, manifest.base_history);
            doc.encode_wire_since(&E::StateVector::default()).len()
        }
        "wire_encode_delta" => {
            let n = base_n(manifest.base_history);
            let k = lag_k(manifest.base_history).expect("delta lag");
            let mut doc = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
            let base_sv = doc.state_vector();
            apply_lag_keystrokes(&mut doc, n, k);
            doc.encode_wire_since(&base_sv).len()
        }
        "wire_decode_apply" => {
            let (_target, bytes) = setup_decode_apply::<E>(manifest);
            bytes.len()
        }
        "two_peer_round_trip" => {
            let mut input = setup_two_peer_round_trip::<E>(manifest);
            let (da, db) = measure_two_peer_round_trip(&mut input);
            da.len().saturating_add(db.len())
        }
        "multi_peer_fan_in" => {
            let (_sink, updates) = setup_multi_peer_fan_in::<E>(manifest);
            updates.iter().map(Vec::len).sum::<usize>()
        }
        other => panic!("no wire sample for {other}"),
    }
}

/// Prepare Tier B integrate: target restored outside timer; update decoded outside timer.
pub fn setup_integrate_decoded<E: ComparisonAdapter>(
    manifest: &ScenarioManifest,
) -> (E, E::DecodedUpdate)
where
    E::StateVector: Default + Clone,
{
    match lag_k(manifest.base_history) {
        None => {
            let source = build_from_history::<E>(manifest.peer_a, manifest.base_history);
            let update = source.export_decoded_since(&E::StateVector::default());
            (E::empty(manifest.peer_b), update)
        }
        Some(k) => {
            let n = base_n(manifest.base_history);
            let mut source = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
            let mut target = E::empty(manifest.peer_b);
            target.apply_decoded(source.export_decoded_since(&E::StateVector::default()));
            let base_sv = source.state_vector();
            apply_lag_keystrokes(&mut source, n, k);
            let update = source.export_decoded_since(&base_sv);
            (target, update)
        }
    }
}

/// Timed integrate of a pre-decoded update.
pub fn measure_integrate_decoded<E: ComparisonAdapter>(target: &mut E, update: E::DecodedUpdate) {
    target.apply_decoded(update);
}

/// Setup for wire encode full: immutable source restored outside timer.
pub fn setup_encode_full<E: ComparisonAdapter>(manifest: &ScenarioManifest) -> E {
    build_from_history::<E>(manifest.peer_a, manifest.base_history)
}

/// Timed full wire encode.
pub fn measure_encode_full<E: ComparisonAdapter>(source: &mut E) -> Vec<u8>
where
    E::StateVector: Default,
{
    source.encode_wire_since(&E::StateVector::default())
}

/// Setup for wire encode delta: source has lag applied; base SV captured in setup.
pub fn setup_encode_delta<E: ComparisonAdapter>(
    manifest: &ScenarioManifest,
) -> (E, E::StateVector) {
    let n = base_n(manifest.base_history);
    let k = lag_k(manifest.base_history).expect("k");
    let mut source = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
    let base_sv = source.state_vector();
    apply_lag_keystrokes(&mut source, n, k);
    (source, base_sv)
}

/// Timed delta wire encode.
pub fn measure_encode_delta<E: ComparisonAdapter>(
    source: &mut E,
    base_sv: &E::StateVector,
) -> Vec<u8> {
    source.encode_wire_since(base_sv)
}

/// Setup for wire decode+apply: target + prebuilt codec bytes (decode timed).
pub fn setup_decode_apply<E: ComparisonAdapter>(manifest: &ScenarioManifest) -> (E, Vec<u8>)
where
    E::StateVector: Default + Clone,
{
    match lag_k(manifest.base_history) {
        None => {
            let source = build_from_history::<E>(manifest.peer_a, manifest.base_history);
            let bytes = source.encode_wire_since(&E::StateVector::default());
            (E::empty(manifest.peer_b), bytes)
        }
        Some(k) => {
            let n = base_n(manifest.base_history);
            let mut source = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
            let mut target = E::empty(manifest.peer_b);
            target.apply_decoded(source.export_decoded_since(&E::StateVector::default()));
            let base_sv = source.state_vector();
            apply_lag_keystrokes(&mut source, n, k);
            let bytes = source.encode_wire_since(&base_sv);
            (target, bytes)
        }
    }
}

/// Timed decode + apply.
pub fn measure_decode_apply<E: ComparisonAdapter>(target: &mut E, bytes: &[u8]) {
    let update = E::decode_wire(bytes);
    target.apply_decoded(update);
}

/// Two-peer concurrent round-trip state after untimed concurrent inserts.
pub struct TwoPeerInput<E: ComparisonAdapter> {
    pub peer_a: E,
    pub peer_b: E,
    pub sv_a: E::StateVector,
    pub sv_b: E::StateVector,
}

/// Setup two-peer schedule entirely outside the timer.
pub fn setup_two_peer_round_trip<E: ComparisonAdapter>(
    manifest: &ScenarioManifest,
) -> TwoPeerInput<E>
where
    E::StateVector: Default + Clone,
{
    let n = base_n(manifest.base_history);
    let idx = middle_index(n);
    let mut peer_a = E::restore(&E::seed(manifest.peer_a, &fill_text(n)));
    let mut peer_b = E::empty(manifest.peer_b);
    peer_b.apply_decoded(peer_a.export_decoded_since(&E::StateVector::default()));
    let sv_a = peer_a.state_vector();
    let sv_b = peer_b.state_vector();
    peer_a.insert_at(idx, PEER_A_MARKER);
    peer_b.insert_at(idx, PEER_B_MARKER);
    TwoPeerInput {
        peer_a,
        peer_b,
        sv_a,
        sv_b,
    }
}

/// Timed encode/decode/apply both concurrent deltas.
pub fn measure_two_peer_round_trip<E: ComparisonAdapter>(
    input: &mut TwoPeerInput<E>,
) -> (Vec<u8>, Vec<u8>) {
    let bytes_a = input.peer_a.encode_wire_since(&input.sv_b);
    let bytes_b = input.peer_b.encode_wire_since(&input.sv_a);
    let update_for_b = E::decode_wire(&bytes_a);
    let update_for_a = E::decode_wire(&bytes_b);
    input.peer_b.apply_decoded(update_for_b);
    input.peer_a.apply_decoded(update_for_a);
    (bytes_a, bytes_b)
}

/// Assert per-engine convergence and unique concurrent markers.
pub fn assert_two_peer_markers<E: ComparisonAdapter>(input: &TwoPeerInput<E>) {
    let a = input.peer_a.visible_string();
    let b = input.peer_b.visible_string();
    assert_eq!(a, b, "peers must converge within one engine");
    assert_eq!(
        a.matches(PEER_A_MARKER).count(),
        1,
        "missing marker a in {a}"
    );
    assert_eq!(
        a.matches(PEER_B_MARKER).count(),
        1,
        "missing marker b in {a}"
    );
}

/// Setup multi-peer fan-in: sink at base; prebuilt remote wire updates (outside timer).
pub fn setup_multi_peer_fan_in<E: ComparisonAdapter>(
    manifest: &ScenarioManifest,
) -> (E, Vec<Vec<u8>>)
where
    E::StateVector: Default + Clone,
{
    let n = base_n(manifest.base_history);
    let peers = match &manifest.expectation {
        crate::scenario::SequentialExpectation::FanInMarkers { peer_count } => *peer_count,
        _ => panic!("multi_peer_fan_in requires FanInMarkers expectation"),
    };
    let sink = E::restore(&E::seed(1, &fill_text(n)));
    let base_sv = sink.state_vector();
    let base_wire = sink.encode_wire_since(&E::StateVector::default());
    let mut updates = Vec::with_capacity(peers.saturating_sub(1));
    for p in 2..=peers as u64 {
        let mut peer = E::empty(p);
        peer.apply_decoded(E::decode_wire(&base_wire));
        let marker = peer_marker(p);
        peer.insert_at(n, &marker);
        updates.push(peer.encode_wire_since(&base_sv));
    }
    (sink, updates)
}

/// Timed fan-in: decode+apply every remote update onto the sink.
pub fn measure_multi_peer_fan_in<E: ComparisonAdapter>(sink: &mut E, updates: &[Vec<u8>]) {
    for bytes in updates {
        measure_decode_apply(sink, bytes);
    }
}

/// Assert each remote peer marker appears once on the fan-in sink.
pub fn assert_fan_in_markers<E: ComparisonAdapter>(sink: &E, peer_count: usize, base_n: usize) {
    let visible = sink.visible_string();
    for p in 2..=peer_count as u64 {
        let m = peer_marker(p);
        assert_eq!(
            visible.matches(m.as_str()).count(),
            1,
            "missing/duplicate marker {m} in {visible}"
        );
    }
    assert_eq!(
        visible.len(),
        base_n.saturating_add(peer_count.saturating_sub(1))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{SequentialExpectation, Workload, all_v1_manifests};
    use crate::{MdCrdtAdapter, YrsAdapter};

    fn public_text_ok<E: ComparisonAdapter>() {
        for manifest in all_v1_manifests()
            .into_iter()
            .filter(|m| m.workload_id.starts_with("text_"))
        {
            let got = run_public_text_once::<E>(&manifest);
            match &manifest.expectation {
                SequentialExpectation::ExactVisible(exp) => assert_eq!(&got, exp),
                SequentialExpectation::ConcurrentMarkers { .. }
                | SequentialExpectation::FanInMarkers { .. } => panic!("text is sequential"),
            }
        }
    }

    #[test]
    fn public_text_runners_both_engines() {
        public_text_ok::<MdCrdtAdapter>();
        public_text_ok::<YrsAdapter>();
    }

    fn integrate_cases<E: ComparisonAdapter>()
    where
        E::StateVector: Default + Clone,
    {
        for lag in [None, Some(1usize)] {
            let manifest = Workload::IntegrateDecodedUpdate { n: 32, lag }.to_manifest();
            let (mut target, update) = setup_integrate_decoded::<E>(&manifest);
            measure_integrate_decoded(&mut target, update);
            match &manifest.expectation {
                SequentialExpectation::ExactVisible(exp) => {
                    assert_eq!(target.visible_string(), *exp);
                }
                SequentialExpectation::ConcurrentMarkers { .. }
                | SequentialExpectation::FanInMarkers { .. } => panic!("unexpected"),
            }
        }
    }

    #[test]
    fn integrate_full_and_delta_both_engines() {
        integrate_cases::<MdCrdtAdapter>();
        integrate_cases::<YrsAdapter>();
    }

    fn wire_smoke<E: ComparisonAdapter>()
    where
        E::StateVector: Default + Clone,
    {
        let full = Workload::WireEncodeFull { n: 32 }.to_manifest();
        let mut source = setup_encode_full::<E>(&full);
        let bytes = measure_encode_full(&mut source);
        assert!(!bytes.is_empty());
        assert_eq!(sample_wire_payload_bytes::<E>(&full), bytes.len());

        let delta = Workload::WireEncodeDelta { n: 32, k: 1 }.to_manifest();
        let (mut source, base_sv) = setup_encode_delta::<E>(&delta);
        assert!(!measure_encode_delta(&mut source, &base_sv).is_empty());

        let dec = Workload::WireDecodeApply {
            n: 32,
            lag: Some(1),
        }
        .to_manifest();
        let (mut target, bytes) = setup_decode_apply::<E>(&dec);
        measure_decode_apply(&mut target, &bytes);
        match &dec.expectation {
            SequentialExpectation::ExactVisible(exp) => {
                assert_eq!(target.visible_string(), *exp);
            }
            SequentialExpectation::ConcurrentMarkers { .. }
            | SequentialExpectation::FanInMarkers { .. } => panic!("unexpected"),
        }
    }

    #[test]
    fn wire_encode_and_decode_apply_smoke() {
        wire_smoke::<MdCrdtAdapter>();
        wire_smoke::<YrsAdapter>();
    }

    fn two_peer<E: ComparisonAdapter>()
    where
        E::StateVector: Default + Clone,
    {
        let manifest = Workload::TwoPeerRoundTrip { n: 32 }.to_manifest();
        let mut input = setup_two_peer_round_trip::<E>(&manifest);
        let _ = measure_two_peer_round_trip(&mut input);
        assert_two_peer_markers(&input);
    }

    #[test]
    fn two_peer_round_trip_both_engines() {
        two_peer::<MdCrdtAdapter>();
        two_peer::<YrsAdapter>();
    }

    fn paste_and_fan_in<E: ComparisonAdapter>()
    where
        E::StateVector: Default + Clone,
    {
        let paste = Workload::TextPasteMiddle { n: 64, r: 16 }.to_manifest();
        let got = run_public_text_once::<E>(&paste);
        match paste.expectation {
            SequentialExpectation::ExactVisible(exp) => assert_eq!(got, exp),
            _ => panic!("paste sequential"),
        }

        let fan = Workload::MultiPeerFanIn { n: 64, peers: 4 }.to_manifest();
        let (mut sink, updates) = setup_multi_peer_fan_in::<E>(&fan);
        measure_multi_peer_fan_in(&mut sink, &updates);
        assert_fan_in_markers(&sink, 4, 64);
        assert_eq!(
            sample_wire_payload_bytes::<E>(&fan),
            updates.iter().map(Vec::len).sum::<usize>()
        );
    }

    #[test]
    fn stretch_paste_and_fan_in_both_engines() {
        paste_and_fan_in::<MdCrdtAdapter>();
        paste_and_fan_in::<YrsAdapter>();
    }
}
