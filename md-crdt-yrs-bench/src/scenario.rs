//! Workload definitions and immutable scenario manifests.

use crate::sizes::{
    FANIN_BASE_N, FANIN_PEER_COUNTS, KEYSTROKE_BYTE, MIDDLE_INSERT, PASTE_LENS, PEER_A_MARKER,
    PEER_B_MARKER, V1_SIZE_MATRIX, append_run_payload, fill_text, keystroke_payload, middle_index,
    paste_payload, peer_a, peer_b, peer_marker,
};
use crate::{MD_CRDT_WIRE_CODEC, YRS_TEXT_ROOT, YRS_WIRE_CODEC};

/// Claim tier for a benchmark id. Cross-tier ratios are forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Public collaborative text operations (layer-inclusive).
    PublicText,
    /// Apply already-decoded updates only.
    DecodedIntegration,
    /// Declared serialization + decode + apply pipelines.
    WirePipeline,
}

impl Tier {
    /// Path segment used in Criterion ids.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::PublicText => "public_text",
            Self::DecodedIntegration => "decoded_integration",
            Self::WirePipeline => "wire_pipeline",
        }
    }
}

/// Engine id for side-by-side groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineId {
    MdCrdt,
    Yrs,
}

impl EngineId {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::MdCrdt => "md_crdt",
            Self::Yrs => "yrs",
        }
    }

    #[must_use]
    pub const fn wire_codec(self) -> &'static str {
        match self {
            Self::MdCrdt => MD_CRDT_WIRE_CODEC,
            Self::Yrs => YRS_WIRE_CODEC,
        }
    }
}

/// How Criterion should batch setup vs measurement for this scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchPolicy {
    /// Destructive or allocate-heavy cases: restore/decode outside the timer;
    /// maps to Criterion `BatchSize::LargeInput` when benches land.
    LargeInput,
}

/// How the base document history is constructed before timed work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseHistory {
    /// One bulk insert of N fill bytes (one API call, one Yrs transaction).
    BulkSeed { n: usize },
    /// Bulk seed of N, then K one-byte appends (K API calls / K Yrs transactions).
    BulkSeedPlusLag { n: usize, k: usize },
}

impl BaseHistory {
    #[must_use]
    pub const fn visible_n(self) -> usize {
        match self {
            Self::BulkSeed { n } => n,
            Self::BulkSeedPlusLag { n, k } => n.saturating_add(k),
        }
    }

    /// API calls used to build the base history (seed path only).
    #[must_use]
    pub const fn api_calls(self) -> usize {
        match self {
            Self::BulkSeed { .. } => 1,
            Self::BulkSeedPlusLag { k, .. } => 1usize.saturating_add(k),
        }
    }

    /// Yrs write transactions used to build the base history.
    #[must_use]
    pub const fn yrs_transactions(self) -> usize {
        self.api_calls()
    }
}

/// Post-condition checked after sequential scenarios (exact text-container body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequentialExpectation {
    /// Exact visible string after the timed (or full scripted) operation.
    ExactVisible(String),
    /// Not a single global string (concurrent markers only).
    ConcurrentMarkers {
        marker_a: &'static str,
        marker_b: &'static str,
    },
    /// Multi-peer fan-in: each peer marker appears exactly once after merge.
    FanInMarkers { peer_count: usize },
}

/// Workload kind before expansion into a full [`ScenarioManifest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Workload {
    TextInsertMiddle {
        n: usize,
    },
    TextAppendRun {
        n: usize,
        m: usize,
    },
    TextAppendKeystrokes {
        n: usize,
        m: usize,
    },
    TextDeleteMiddle {
        n: usize,
    },
    IntegrateDecodedUpdate {
        n: usize,
        /// `None` => history=full; `Some(k)` => history=delta with lag k.
        lag: Option<usize>,
    },
    WireEncodeFull {
        n: usize,
    },
    WireEncodeDelta {
        n: usize,
        k: usize,
    },
    WireDecodeApply {
        n: usize,
        lag: Option<usize>,
    },
    TwoPeerRoundTrip {
        n: usize,
    },
    /// Stretch: single public call pasting R bytes at middle of N.
    TextPasteMiddle {
        n: usize,
        r: usize,
    },
    /// Stretch: P peers insert unique markers; sink applies all remote updates.
    MultiPeerFanIn {
        n: usize,
        peers: usize,
    },
}

impl Workload {
    #[must_use]
    pub const fn workload_id(self) -> &'static str {
        match self {
            Self::TextInsertMiddle { .. } => "text_insert_middle",
            Self::TextAppendRun { .. } => "text_append_run",
            Self::TextAppendKeystrokes { .. } => "text_append_keystrokes",
            Self::TextDeleteMiddle { .. } => "text_delete_middle",
            Self::IntegrateDecodedUpdate { .. } => "integrate_decoded_update",
            Self::WireEncodeFull { .. } => "wire_encode_full",
            Self::WireEncodeDelta { .. } => "wire_encode_delta",
            Self::WireDecodeApply { .. } => "wire_decode_apply",
            Self::TwoPeerRoundTrip { .. } => "two_peer_round_trip",
            Self::TextPasteMiddle { .. } => "text_paste_middle",
            Self::MultiPeerFanIn { .. } => "multi_peer_fan_in",
        }
    }

    #[must_use]
    pub const fn tier(self) -> Tier {
        match self {
            Self::TextInsertMiddle { .. }
            | Self::TextAppendRun { .. }
            | Self::TextAppendKeystrokes { .. }
            | Self::TextDeleteMiddle { .. }
            | Self::TextPasteMiddle { .. } => Tier::PublicText,
            Self::IntegrateDecodedUpdate { .. } => Tier::DecodedIntegration,
            Self::WireEncodeFull { .. }
            | Self::WireEncodeDelta { .. }
            | Self::WireDecodeApply { .. }
            | Self::TwoPeerRoundTrip { .. }
            | Self::MultiPeerFanIn { .. } => Tier::WirePipeline,
        }
    }

    /// Expand this workload into a frozen [`ScenarioManifest`].
    #[must_use]
    pub fn to_manifest(self) -> ScenarioManifest {
        match self {
            Self::TextInsertMiddle { n } => text_insert_middle(n),
            Self::TextAppendRun { n, m } => text_append_run(n, m),
            Self::TextAppendKeystrokes { n, m } => text_append_keystrokes(n, m),
            Self::TextDeleteMiddle { n } => text_delete_middle(n),
            Self::IntegrateDecodedUpdate { n, lag } => integrate_decoded_update(n, lag),
            Self::WireEncodeFull { n } => wire_encode_full(n),
            Self::WireEncodeDelta { n, k } => wire_encode_delta(n, k),
            Self::WireDecodeApply { n, lag } => wire_decode_apply(n, lag),
            Self::TwoPeerRoundTrip { n } => two_peer_round_trip(n),
            Self::TextPasteMiddle { n, r } => text_paste_middle(n, r),
            Self::MultiPeerFanIn { n, peers } => multi_peer_fan_in(n, peers),
        }
    }
}

/// Immutable record of everything a controlled case freezes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioManifest {
    pub tier: Tier,
    pub workload_id: &'static str,
    /// Criterion parameter / parameter id (not including tier or engine).
    pub parameter_id: String,
    pub peer_a: u64,
    pub peer_b: u64,
    pub yrs_text_root: &'static str,
    pub base_history: BaseHistory,
    /// Timed logical operations (e.g. 1 insert, M keystrokes, 1 encode).
    pub timed_logical_ops: usize,
    /// Timed public API calls on md-crdt (and matching Yrs call count).
    pub timed_api_calls: usize,
    /// Timed Yrs write transactions (0 for pure encode/read paths).
    pub timed_yrs_transactions: usize,
    /// Edit payload used by the primary timed mutation, when applicable.
    pub edit_payload: Option<String>,
    /// Index for middle insert/delete, when applicable.
    pub edit_index: Option<usize>,
    pub batch_policy: BatchPolicy,
    /// Codec label when the scenario involves wire bytes (Tier C, or prep for B).
    pub codec_md_crdt: Option<&'static str>,
    pub codec_yrs: Option<&'static str>,
    pub expectation: SequentialExpectation,
}

impl ScenarioManifest {
    /// Full Criterion group path prefix: `{tier}/{workload}`.
    #[must_use]
    pub fn group_path(&self) -> String {
        format!("{}/{}", self.tier.id(), self.workload_id)
    }

    /// Id used as Criterion `BenchmarkId` function/parameter text (engine separate).
    #[must_use]
    pub fn benchmark_parameter(&self) -> &str {
        &self.parameter_id
    }
}

fn text_insert_middle(n: usize) -> ScenarioManifest {
    let idx = middle_index(n);
    let expected = format!(
        "{}{}{}",
        fill_text(idx),
        MIDDLE_INSERT,
        fill_text(n.saturating_sub(idx))
    );
    ScenarioManifest {
        tier: Tier::PublicText,
        workload_id: "text_insert_middle",
        parameter_id: format!("n={n}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 1,
        edit_payload: Some(MIDDLE_INSERT.to_owned()),
        edit_index: Some(idx),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn text_append_run(n: usize, m: usize) -> ScenarioManifest {
    let payload = append_run_payload(m);
    let expected = format!("{}{payload}", fill_text(n));
    ScenarioManifest {
        tier: Tier::PublicText,
        workload_id: "text_append_run",
        parameter_id: format!("n={n},m={m}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 1,
        edit_payload: Some(payload),
        edit_index: Some(n),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn text_append_keystrokes(n: usize, m: usize) -> ScenarioManifest {
    let mut expected = fill_text(n);
    for _ in 0..m {
        expected.push(char::from(KEYSTROKE_BYTE));
    }
    ScenarioManifest {
        tier: Tier::PublicText,
        workload_id: "text_append_keystrokes",
        parameter_id: format!("n={n},m={m}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: m,
        timed_api_calls: m,
        timed_yrs_transactions: m,
        edit_payload: Some(keystroke_payload().to_owned()),
        edit_index: Some(n),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn text_delete_middle(n: usize) -> ScenarioManifest {
    assert!(n >= 1, "delete-middle requires n >= 1");
    let idx = middle_index(n);
    let mut body = fill_text(n);
    body.remove(idx);
    ScenarioManifest {
        tier: Tier::PublicText,
        workload_id: "text_delete_middle",
        parameter_id: format!("n={n}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 1,
        edit_payload: None,
        edit_index: Some(idx),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(body),
    }
}

fn history_param(n: usize, lag: Option<usize>) -> String {
    match lag {
        None => format!("history=full,n={n}"),
        Some(k) => format!("history=delta,n={n},k={k}"),
    }
}

fn base_for_lag(n: usize, lag: Option<usize>) -> BaseHistory {
    match lag {
        None => BaseHistory::BulkSeed { n },
        Some(k) => BaseHistory::BulkSeedPlusLag { n, k },
    }
}

fn integrate_decoded_update(n: usize, lag: Option<usize>) -> ScenarioManifest {
    // Timed work is integration only; expectation is the source body after apply.
    let base = base_for_lag(n, lag);
    let expected = match lag {
        None => fill_text(n),
        Some(k) => {
            let mut s = fill_text(n);
            s.push_str(&append_run_payload(k));
            s
        }
    };
    ScenarioManifest {
        tier: Tier::DecodedIntegration,
        workload_id: "integrate_decoded_update",
        parameter_id: history_param(n, lag),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: base,
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 1,
        edit_payload: None,
        edit_index: None,
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn wire_encode_full(n: usize) -> ScenarioManifest {
    ScenarioManifest {
        tier: Tier::WirePipeline,
        workload_id: "wire_encode_full",
        parameter_id: format!("n={n}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 0,
        edit_payload: None,
        edit_index: None,
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: Some(MD_CRDT_WIRE_CODEC),
        codec_yrs: Some(YRS_WIRE_CODEC),
        // Encode does not mutate visible text.
        expectation: SequentialExpectation::ExactVisible(fill_text(n)),
    }
}

fn wire_encode_delta(n: usize, k: usize) -> ScenarioManifest {
    let mut expected = fill_text(n);
    expected.push_str(&append_run_payload(k));
    ScenarioManifest {
        tier: Tier::WirePipeline,
        workload_id: "wire_encode_delta",
        parameter_id: format!("n={n},k={k}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeedPlusLag { n, k },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 0,
        edit_payload: None,
        edit_index: None,
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: Some(MD_CRDT_WIRE_CODEC),
        codec_yrs: Some(YRS_WIRE_CODEC),
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn wire_decode_apply(n: usize, lag: Option<usize>) -> ScenarioManifest {
    let base = base_for_lag(n, lag);
    let expected = match lag {
        None => fill_text(n),
        Some(k) => {
            let mut s = fill_text(n);
            s.push_str(&append_run_payload(k));
            s
        }
    };
    ScenarioManifest {
        tier: Tier::WirePipeline,
        workload_id: "wire_decode_apply",
        parameter_id: history_param(n, lag),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: base,
        timed_logical_ops: 1,
        timed_api_calls: 1,
        // Decode+apply includes Yrs commit on the apply side.
        timed_yrs_transactions: 1,
        edit_payload: None,
        edit_index: None,
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: Some(MD_CRDT_WIRE_CODEC),
        codec_yrs: Some(YRS_WIRE_CODEC),
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn two_peer_round_trip(n: usize) -> ScenarioManifest {
    let idx = middle_index(n);
    ScenarioManifest {
        tier: Tier::WirePipeline,
        workload_id: "two_peer_round_trip",
        parameter_id: format!("n={n}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        // Encode+decode+apply both directions.
        timed_logical_ops: 2,
        timed_api_calls: 2,
        timed_yrs_transactions: 2,
        edit_payload: Some(format!("{PEER_A_MARKER}/{PEER_B_MARKER}")),
        edit_index: Some(idx),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: Some(MD_CRDT_WIRE_CODEC),
        codec_yrs: Some(YRS_WIRE_CODEC),
        expectation: SequentialExpectation::ConcurrentMarkers {
            marker_a: PEER_A_MARKER,
            marker_b: PEER_B_MARKER,
        },
    }
}

fn text_paste_middle(n: usize, r: usize) -> ScenarioManifest {
    let idx = middle_index(n);
    let payload = paste_payload(r);
    let expected = format!(
        "{}{}{}",
        fill_text(idx),
        payload,
        fill_text(n.saturating_sub(idx))
    );
    ScenarioManifest {
        tier: Tier::PublicText,
        workload_id: "text_paste_middle",
        parameter_id: format!("n={n},r={r}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: 1,
        timed_api_calls: 1,
        timed_yrs_transactions: 1,
        edit_payload: Some(payload),
        edit_index: Some(idx),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: None,
        codec_yrs: None,
        expectation: SequentialExpectation::ExactVisible(expected),
    }
}

fn multi_peer_fan_in(n: usize, peers: usize) -> ScenarioManifest {
    assert!(peers >= 2, "fan-in needs at least 2 peers");
    // Sink is peer 1; peers 2..=peers each contribute one remote update.
    let remote = peers.saturating_sub(1);
    ScenarioManifest {
        tier: Tier::WirePipeline,
        workload_id: "multi_peer_fan_in",
        parameter_id: format!("n={n},peers={peers}"),
        peer_a: peer_a(),
        peer_b: peer_b(),
        yrs_text_root: YRS_TEXT_ROOT,
        base_history: BaseHistory::BulkSeed { n },
        timed_logical_ops: remote,
        timed_api_calls: remote,
        timed_yrs_transactions: remote,
        edit_payload: Some(
            (2..=peers as u64)
                .map(peer_marker)
                .collect::<Vec<_>>()
                .join(""),
        ),
        edit_index: Some(n),
        batch_policy: BatchPolicy::LargeInput,
        codec_md_crdt: Some(MD_CRDT_WIRE_CODEC),
        codec_yrs: Some(YRS_WIRE_CODEC),
        expectation: SequentialExpectation::FanInMarkers { peer_count: peers },
    }
}

/// Pure expected string for a middle insert of [`MIDDLE_INSERT`] into `n` fill bytes.
#[must_use]
pub fn expected_after_middle_insert(n: usize) -> String {
    match text_insert_middle(n).expectation {
        SequentialExpectation::ExactVisible(s) => s,
        SequentialExpectation::ConcurrentMarkers { .. }
        | SequentialExpectation::FanInMarkers { .. } => {
            unreachable!("middle insert is sequential")
        }
    }
}

/// All v1 workloads before expansion (Cartesian product over the size matrix).
#[must_use]
pub fn all_v1_workloads() -> Vec<Workload> {
    let mut out = Vec::new();
    for &n in V1_SIZE_MATRIX.text_lens {
        out.push(Workload::TextInsertMiddle { n });
        out.push(Workload::TextDeleteMiddle { n });
        out.push(Workload::WireEncodeFull { n });
        out.push(Workload::IntegrateDecodedUpdate { n, lag: None });
        out.push(Workload::WireDecodeApply { n, lag: None });
        out.push(Workload::TwoPeerRoundTrip { n });
        for &m in V1_SIZE_MATRIX.append_lens {
            out.push(Workload::TextAppendRun { n, m });
            out.push(Workload::TextAppendKeystrokes { n, m });
        }
        for &k in V1_SIZE_MATRIX.delta_lags {
            out.push(Workload::IntegrateDecodedUpdate { n, lag: Some(k) });
            out.push(Workload::WireEncodeDelta { n, k });
            out.push(Workload::WireDecodeApply { n, lag: Some(k) });
        }
    }
    out
}

/// Every v1 [`ScenarioManifest`].
#[must_use]
pub fn all_v1_manifests() -> Vec<ScenarioManifest> {
    all_v1_workloads()
        .into_iter()
        .map(Workload::to_manifest)
        .collect()
}

/// Stretch workloads (Phase 6 competitive extensions).
#[must_use]
pub fn all_stretch_workloads() -> Vec<Workload> {
    let mut out = Vec::new();
    let n = FANIN_BASE_N;
    for &r in PASTE_LENS {
        out.push(Workload::TextPasteMiddle { n, r });
    }
    for &peers in FANIN_PEER_COUNTS {
        out.push(Workload::MultiPeerFanIn { n, peers });
    }
    out
}

/// Stretch manifests only.
#[must_use]
pub fn all_stretch_manifests() -> Vec<ScenarioManifest> {
    all_stretch_workloads()
        .into_iter()
        .map(Workload::to_manifest)
        .collect()
}

/// v1 + stretch competitive manifests used by the Criterion suite.
#[must_use]
pub fn all_competitive_manifests() -> Vec<ScenarioManifest> {
    let mut out = all_v1_manifests();
    out.extend(all_stretch_manifests());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn text_lens_and_append_and_lags_match_sizes_module() {
        use crate::sizes::{APPEND_LENS, DELTA_LAGS, TEXT_LENS};
        assert_eq!(TEXT_LENS, V1_SIZE_MATRIX.text_lens);
        assert_eq!(APPEND_LENS, V1_SIZE_MATRIX.append_lens);
        assert_eq!(DELTA_LAGS, V1_SIZE_MATRIX.delta_lags);
    }

    #[test]
    fn middle_insert_expected_string_is_exact() {
        let n = 10;
        assert_eq!(
            expected_after_middle_insert(n),
            format!("xxxxx{}xxxxx", MIDDLE_INSERT)
        );
        let manifest = Workload::TextInsertMiddle { n: 1_000 }.to_manifest();
        assert_eq!(manifest.timed_api_calls, 1);
        assert_eq!(manifest.timed_yrs_transactions, 1);
        assert_eq!(manifest.edit_index, Some(500));
        match &manifest.expectation {
            SequentialExpectation::ExactVisible(s) => {
                assert_eq!(s.len(), 1_001);
                assert_eq!(&s[500..501], MIDDLE_INSERT);
                assert!(s.bytes().filter(|&b| b == b'x').count() == 1_000);
            }
            SequentialExpectation::ConcurrentMarkers { .. }
            | SequentialExpectation::FanInMarkers { .. } => panic!("expected exact"),
        }
    }

    #[test]
    fn append_run_and_keystrokes_expectations() {
        let run = Workload::TextAppendRun { n: 4, m: 3 }.to_manifest();
        assert_eq!(
            run.expectation,
            SequentialExpectation::ExactVisible("xxxxzzz".into())
        );
        assert_eq!(run.timed_api_calls, 1);
        assert_eq!(run.timed_yrs_transactions, 1);

        let keys = Workload::TextAppendKeystrokes { n: 4, m: 3 }.to_manifest();
        assert_eq!(
            keys.expectation,
            SequentialExpectation::ExactVisible("xxxxzzz".into())
        );
        assert_eq!(keys.timed_api_calls, 3);
        assert_eq!(keys.timed_yrs_transactions, 3);
    }

    #[test]
    fn delete_middle_expectation() {
        let m = Workload::TextDeleteMiddle { n: 5 }.to_manifest();
        // middle_index(5)=2 → remove one x
        assert_eq!(
            m.expectation,
            SequentialExpectation::ExactVisible("xxxx".into())
        );
        assert_eq!(m.edit_index, Some(2));
    }

    #[test]
    fn base_history_counts() {
        assert_eq!(BaseHistory::BulkSeed { n: 10 }.api_calls(), 1);
        assert_eq!(
            BaseHistory::BulkSeedPlusLag { n: 10, k: 100 }.api_calls(),
            101
        );
        assert_eq!(
            BaseHistory::BulkSeedPlusLag { n: 10, k: 100 }.visible_n(),
            110
        );
    }

    #[test]
    fn every_v1_manifest_has_tier_and_frozen_schedule() {
        let manifests = all_v1_manifests();
        assert!(!manifests.is_empty());
        // 2 text lens *
        //   insert + delete + wire_full + integrate_full + decode_full + two_peer = 6
        //   + 2 append * 2 workloads = 4
        //   + 2 lags * 3 (integrate_delta, wire_delta, decode_delta) = 6
        // = 2 * (6+4+6) = 32
        assert_eq!(manifests.len(), 32);

        let mut ids = BTreeSet::new();
        for m in &manifests {
            assert!(!m.workload_id.is_empty());
            assert!(!m.parameter_id.is_empty());
            assert_eq!(m.peer_a, 1);
            assert_eq!(m.peer_b, 2);
            assert_eq!(m.yrs_text_root, "text");
            assert_eq!(m.batch_policy, BatchPolicy::LargeInput);
            assert!(m.timed_logical_ops >= 1);
            assert!(m.base_history.api_calls() >= 1);
            let key = format!("{}/{}", m.group_path(), m.parameter_id);
            assert!(ids.insert(key.clone()), "duplicate scenario id {key}");
            // Tier C always names codecs; Tier A never does.
            match m.tier {
                Tier::PublicText => {
                    assert!(m.codec_md_crdt.is_none());
                    assert!(m.codec_yrs.is_none());
                }
                Tier::WirePipeline => {
                    assert_eq!(m.codec_md_crdt, Some(MD_CRDT_WIRE_CODEC));
                    assert_eq!(m.codec_yrs, Some(YRS_WIRE_CODEC));
                }
                Tier::DecodedIntegration => {
                    assert!(m.codec_md_crdt.is_none());
                    assert!(m.codec_yrs.is_none());
                }
            }
        }
    }

    #[test]
    fn two_peer_uses_concurrent_markers_not_exact_order() {
        let m = Workload::TwoPeerRoundTrip { n: 1_000 }.to_manifest();
        assert_eq!(m.tier, Tier::WirePipeline);
        assert_eq!(
            m.expectation,
            SequentialExpectation::ConcurrentMarkers {
                marker_a: "a",
                marker_b: "b",
            }
        );
        assert_eq!(m.edit_index, Some(500));
    }

    #[test]
    fn engine_codec_labels() {
        assert_eq!(EngineId::MdCrdt.wire_codec(), "md_crdt_serde_json_v1");
        assert_eq!(EngineId::Yrs.wire_codec(), "yrs_lib0_v1");
    }

    #[test]
    fn stretch_paste_and_fan_in_manifests() {
        let paste = Workload::TextPasteMiddle { n: 1_000, r: 256 }.to_manifest();
        assert_eq!(paste.workload_id, "text_paste_middle");
        assert_eq!(paste.timed_api_calls, 1);
        match &paste.expectation {
            SequentialExpectation::ExactVisible(s) => assert_eq!(s.len(), 1_000 + 256),
            _ => panic!("paste is sequential"),
        }
        let fan = Workload::MultiPeerFanIn { n: 1_000, peers: 4 }.to_manifest();
        assert_eq!(fan.timed_logical_ops, 3);
        assert_eq!(
            fan.expectation,
            SequentialExpectation::FanInMarkers { peer_count: 4 }
        );
        assert_eq!(
            all_stretch_manifests().len(),
            PASTE_LENS.len() + FANIN_PEER_COUNTS.len()
        );
        assert_eq!(
            all_competitive_manifests().len(),
            all_v1_manifests().len() + all_stretch_manifests().len()
        );
    }
}
