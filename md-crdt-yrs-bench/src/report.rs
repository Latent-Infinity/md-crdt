//! Report metadata and provenance schema for competitive cases.

use crate::scenario::{BatchPolicy, EngineId, ScenarioManifest, Tier};

/// Required top-level keys in a multi-run provenance sidecar (S10).
///
/// Scripts and fixture tests share this list so fields cannot drift.
pub const PROVENANCE_REQUIRED_KEYS: &[&str] = &[
    "schema_version",
    "clean_worktree",
    "git_commit",
    "root_lockfile_sha256",
    "compare_lockfile_sha256",
    "yrs_version",
    "yrs_checksum",
    "md_crdt_version",
    "criterion_version",
    "md_crdt_features",
    "yrs_features",
    "yrs_text_root",
    "yrs_offset_kind",
    "yrs_skip_gc",
    "md_crdt_peer_ids",
    "yrs_client_ids",
    "md_crdt_wire_codec",
    "yrs_wire_codec",
    "rustc_verbose",
    "host_triple",
    "target_triple",
    "os",
    "cpu_model",
    "power_mode",
    "batch_policy",
    "timestamp_utc",
    "output_dir",
    "invocations",
];

/// Required keys on each element of `invocations`.
pub const INVOCATION_REQUIRED_KEYS: &[&str] = &[
    "index",
    "engine_order",
    "baseline_name",
    "command",
    "started_utc",
    "finished_utc",
    "status",
];

/// Current provenance schema version written by the report script.
pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;

/// Validate a provenance document against the S10 required-key schema.
pub fn validate_provenance_document(value: &serde_json::Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "provenance root must be a JSON object".to_owned())?;
    for key in PROVENANCE_REQUIRED_KEYS {
        if !obj.contains_key(*key) {
            return Err(format!("missing required provenance key: {key}"));
        }
    }
    let schema = obj
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "schema_version must be a number".to_owned())?;
    if schema != u64::from(PROVENANCE_SCHEMA_VERSION) {
        return Err(format!(
            "unsupported schema_version {schema}; expected {}",
            PROVENANCE_SCHEMA_VERSION
        ));
    }
    let clean = obj
        .get("clean_worktree")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "clean_worktree must be a boolean".to_owned())?;
    if !clean {
        return Err("clean_worktree must be true for citable runs".to_owned());
    }
    let frozen_fields = [
        ("yrs_text_root", "text"),
        ("yrs_offset_kind", "Bytes"),
        ("md_crdt_wire_codec", crate::MD_CRDT_WIRE_CODEC),
        ("yrs_wire_codec", crate::YRS_WIRE_CODEC),
    ];
    for (key, expected) in frozen_fields {
        let actual = obj
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{key} must be a string"))?;
        if actual != expected {
            return Err(format!("{key} must be {expected:?}; got {actual:?}"));
        }
    }
    if obj.get("yrs_skip_gc").and_then(|v| v.as_bool()) != Some(false) {
        return Err("yrs_skip_gc must be false".to_owned());
    }
    let expected_peers = serde_json::json!([crate::PEER_A, crate::PEER_B]);
    for key in ["md_crdt_peer_ids", "yrs_client_ids"] {
        if obj.get(key) != Some(&expected_peers) {
            return Err(format!("{key} must be [1, 2]"));
        }
    }
    let invocations = obj
        .get("invocations")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "invocations must be an array".to_owned())?;
    if invocations.len() < 3 {
        return Err(format!(
            "invocations must contain at least 3 runs; found {}",
            invocations.len()
        ));
    }
    let mut baselines = std::collections::BTreeSet::new();
    for (i, inv) in invocations.iter().enumerate() {
        let inv_obj = inv
            .as_object()
            .ok_or_else(|| format!("invocations[{i}] must be an object"))?;
        for key in INVOCATION_REQUIRED_KEYS {
            if !inv_obj.contains_key(*key) {
                return Err(format!("invocations[{i}] missing key: {key}"));
            }
        }
        let baseline = inv_obj
            .get("baseline_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("invocations[{i}].baseline_name must be a string"))?;
        if !baselines.insert(baseline.to_owned()) {
            return Err(format!(
                "duplicate baseline_name {baseline:?}; runs must not overwrite each other"
            ));
        }
        let order = inv_obj
            .get("engine_order")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("invocations[{i}].engine_order must be a string"))?;
        if order != "md_first" && order != "yrs_first" {
            return Err(format!(
                "invocations[{i}].engine_order must be md_first or yrs_first; got {order}"
            ));
        }
    }
    // Alternating order across the first three invocations.
    let o0 = invocations[0]["engine_order"].as_str().unwrap_or("");
    let o1 = invocations[1]["engine_order"].as_str().unwrap_or("");
    let o2 = invocations[2]["engine_order"].as_str().unwrap_or("");
    if o0 == o1 || o1 == o2 {
        return Err(
            "first three invocations must alternate engine_order (md_first/yrs_first)".to_owned(),
        );
    }
    Ok(())
}

/// Frozen work-size / claim context emitted beside Criterion timings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseMetadata {
    pub tier: &'static str,
    pub workload_id: &'static str,
    pub engine: &'static str,
    pub parameter_id: String,
    pub timed_logical_ops: usize,
    pub timed_api_calls: usize,
    pub timed_yrs_transactions: usize,
    pub batch_policy: BatchPolicy,
    /// Declared codec when wire bytes are in play for this engine.
    pub codec: Option<&'static str>,
    /// Wire payload length in bytes when known (Tier C / prep samples).
    pub wire_payload_bytes: Option<usize>,
    /// Base-history API calls (seed construction, untimed).
    pub base_history_api_calls: usize,
    pub base_history_yrs_transactions: usize,
}

impl CaseMetadata {
    /// Build metadata from a frozen manifest and engine (payload length optional).
    #[must_use]
    pub fn from_manifest(
        manifest: &ScenarioManifest,
        engine: EngineId,
        wire_payload_bytes: Option<usize>,
    ) -> Self {
        let codec = match engine {
            EngineId::MdCrdt => manifest.codec_md_crdt,
            EngineId::Yrs => manifest.codec_yrs,
        };
        Self {
            tier: manifest.tier.id(),
            workload_id: manifest.workload_id,
            engine: engine.id(),
            parameter_id: manifest.parameter_id.clone(),
            timed_logical_ops: manifest.timed_logical_ops,
            timed_api_calls: manifest.timed_api_calls,
            timed_yrs_transactions: manifest.timed_yrs_transactions,
            batch_policy: manifest.batch_policy,
            codec,
            wire_payload_bytes,
            base_history_api_calls: manifest.base_history.api_calls(),
            base_history_yrs_transactions: manifest.base_history.yrs_transactions(),
        }
    }

    /// Criterion function id: `{tier}/{workload}/{engine}`.
    #[must_use]
    pub fn criterion_function_id(&self) -> String {
        format!("{}/{}/{}", self.tier, self.workload_id, self.engine)
    }

    /// True when every required S10-style work-size field is present and coherent.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        if self.tier.is_empty()
            || self.workload_id.is_empty()
            || self.engine.is_empty()
            || self.parameter_id.is_empty()
        {
            return false;
        }
        if self.timed_logical_ops == 0 || self.timed_api_calls == 0 {
            // Encode/decode/apply paths always count ≥1 logical/API step.
            return false;
        }
        if self.base_history_api_calls == 0 {
            return false;
        }
        if self.batch_policy != BatchPolicy::LargeInput {
            return false;
        }
        match self.tier {
            "public_text" | "decoded_integration" => {
                if self.codec.is_some() {
                    return false;
                }
                // Wire length is optional for non-wire tiers.
            }
            "wire_pipeline" => {
                if self.codec.is_none() {
                    return false;
                }
                // Payload may be sampled later; completeness requires codec always.
            }
            _ => return false,
        }
        true
    }

    /// Completeness including a sampled wire payload for Tier C.
    #[must_use]
    pub fn is_complete_with_wire_sample(&self) -> bool {
        if !self.is_complete() {
            return false;
        }
        if self.tier == Tier::WirePipeline.id() {
            matches!(self.wire_payload_bytes, Some(n) if n > 0)
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::all_v1_manifests;

    #[test]
    fn every_manifest_engine_pair_has_complete_metadata() {
        for manifest in all_v1_manifests() {
            for engine in [EngineId::MdCrdt, EngineId::Yrs] {
                let meta = CaseMetadata::from_manifest(&manifest, engine, None);
                assert!(
                    meta.is_complete(),
                    "incomplete metadata for {}/{} {} {:?}",
                    meta.tier,
                    meta.workload_id,
                    meta.parameter_id,
                    engine
                );
                assert_eq!(
                    meta.criterion_function_id(),
                    format!(
                        "{}/{}/{}",
                        manifest.tier.id(),
                        manifest.workload_id,
                        engine.id()
                    )
                );
                if manifest.tier == Tier::WirePipeline {
                    assert!(meta.codec.is_some());
                    // Without sample, wire sample completeness is false.
                    assert!(!meta.is_complete_with_wire_sample());
                    let with = CaseMetadata::from_manifest(&manifest, engine, Some(128));
                    assert!(with.is_complete_with_wire_sample());
                }
            }
        }
    }

    #[test]
    fn public_text_rejects_codec_and_wire_sample_not_required() {
        let manifest = all_v1_manifests()
            .into_iter()
            .find(|m| m.workload_id == "text_insert_middle")
            .expect("insert");
        let meta = CaseMetadata::from_manifest(&manifest, EngineId::MdCrdt, None);
        assert!(meta.is_complete());
        assert!(meta.is_complete_with_wire_sample());
        assert!(meta.codec.is_none());
    }

    #[test]
    fn provenance_fixture_satisfies_s10_schema() {
        let raw = include_str!("../tests/fixtures/provenance_valid.json");
        let value: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        validate_provenance_document(&value).expect("valid fixture");
    }

    #[test]
    fn provenance_rejects_missing_key_dirty_tree_and_duplicate_baseline() {
        let raw = include_str!("../tests/fixtures/provenance_valid.json");
        let mut value: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        value
            .as_object_mut()
            .unwrap()
            .remove("git_commit")
            .expect("git_commit present");
        assert!(validate_provenance_document(&value).is_err());

        let mut dirty: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        dirty["clean_worktree"] = serde_json::json!(false);
        assert!(validate_provenance_document(&dirty).is_err());

        let mut dup: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        dup["invocations"][1]["baseline_name"] = dup["invocations"][0]["baseline_name"].clone();
        assert!(
            validate_provenance_document(&dup)
                .unwrap_err()
                .contains("duplicate baseline_name")
        );

        let mut same_order: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        same_order["invocations"][1]["engine_order"] = serde_json::json!("md_first");
        same_order["invocations"][2]["engine_order"] = serde_json::json!("md_first");
        assert!(
            validate_provenance_document(&same_order)
                .unwrap_err()
                .contains("alternate")
        );

        let mut wrong_config: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        wrong_config["yrs_skip_gc"] = serde_json::json!(true);
        assert!(
            validate_provenance_document(&wrong_config)
                .unwrap_err()
                .contains("yrs_skip_gc")
        );
    }

    #[test]
    fn provenance_required_key_lists_are_nonempty_and_unique() {
        assert!(PROVENANCE_REQUIRED_KEYS.len() >= 15);
        assert!(INVOCATION_REQUIRED_KEYS.len() >= 6);
        let mut seen = std::collections::BTreeSet::new();
        for k in PROVENANCE_REQUIRED_KEYS {
            assert!(seen.insert(*k), "duplicate provenance key {k}");
        }
    }
}
