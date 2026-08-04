//! Competitive Criterion suite: md-crdt vs Yrs, tiered ids, batched setup.

use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use md_crdt::{CollaborativeDocument, EquivalenceMode};
use md_crdt_yrs_bench::report::CaseMetadata;
use md_crdt_yrs_bench::runners::{
    measure_decode_apply, measure_encode_delta, measure_encode_full, measure_integrate_decoded,
    measure_multi_peer_fan_in, measure_two_peer_round_trip, run_public_text_once,
    sample_wire_payload_bytes, setup_decode_apply, setup_encode_delta, setup_encode_full,
    setup_integrate_decoded, setup_multi_peer_fan_in, setup_two_peer_round_trip,
};
use md_crdt_yrs_bench::scenario::{
    BatchPolicy, EngineId, ScenarioManifest, all_competitive_manifests,
};
use md_crdt_yrs_bench::sizes::fill_text;
use md_crdt_yrs_bench::{ComparisonAdapter, MdCrdtAdapter, YrsAdapter, keystroke_payload};

fn criterion_batch(policy: BatchPolicy) -> BatchSize {
    match policy {
        BatchPolicy::LargeInput => BatchSize::LargeInput,
    }
}

fn function_id(manifest: &ScenarioManifest, engine: EngineId) -> String {
    format!(
        "{}/{}/{}",
        manifest.tier.id(),
        manifest.workload_id,
        engine.id()
    )
}

fn note_metadata(meta: &CaseMetadata) {
    // Printed once per case registration for report context (not a citable timing).
    eprintln!(
        "case_metadata tier={} workload={} engine={} param={} ops={} api={} yrs_tx={} batch={:?} codec={:?} wire_bytes={:?}",
        meta.tier,
        meta.workload_id,
        meta.engine,
        meta.parameter_id,
        meta.timed_logical_ops,
        meta.timed_api_calls,
        meta.timed_yrs_transactions,
        meta.batch_policy,
        meta.codec,
        meta.wire_payload_bytes
    );
}

fn bench_public_text<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) {
    let meta = CaseMetadata::from_manifest(manifest, engine, None);
    assert!(meta.is_complete());
    note_metadata(&meta);
    group.throughput(Throughput::Elements(manifest.timed_logical_ops as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            let n = match manifest.base_history {
                md_crdt_yrs_bench::BaseHistory::BulkSeed { n }
                | md_crdt_yrs_bench::BaseHistory::BulkSeedPlusLag { n, .. } => n,
            };
            let seed = E::seed(manifest.peer_a, &fill_text(n));
            b.iter_batched_ref(
                || E::restore(&seed),
                |doc| {
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
                        other => panic!("unexpected public text workload {other}"),
                    }
                    // Preserve the mutated document without timing an engine-
                    // specific read transaction or materialization step.
                    black_box(&*doc);
                },
                batch,
            );
        },
    );
    // Keep run_public_text_once referenced for parity with unit runners.
    let _ = run_public_text_once::<E>;
}

fn bench_integrate<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let meta = CaseMetadata::from_manifest(manifest, engine, None);
    assert!(meta.is_complete());
    note_metadata(&meta);
    group.throughput(Throughput::Elements(1));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || {
                    let (target, update) = setup_integrate_decoded::<E>(manifest);
                    (target, Some(update))
                },
                |(target, update)| {
                    let update = update.take().expect("one update per setup");
                    measure_integrate_decoded(target, update);
                    black_box(&*target);
                },
                batch,
            );
        },
    );
}

fn bench_encode_full<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let wire_len = sample_wire_payload_bytes::<E>(manifest);
    let meta = CaseMetadata::from_manifest(manifest, engine, Some(wire_len));
    assert!(meta.is_complete_with_wire_sample());
    note_metadata(&meta);
    group.throughput(Throughput::Bytes(wire_len as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || setup_encode_full::<E>(manifest),
                |source| black_box(measure_encode_full(source)),
                batch,
            );
        },
    );
}

fn bench_encode_delta<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let wire_len = sample_wire_payload_bytes::<E>(manifest);
    let meta = CaseMetadata::from_manifest(manifest, engine, Some(wire_len));
    assert!(meta.is_complete_with_wire_sample());
    note_metadata(&meta);
    group.throughput(Throughput::Bytes(wire_len as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || setup_encode_delta::<E>(manifest),
                |(source, base_sv)| black_box(measure_encode_delta(source, base_sv)),
                batch,
            );
        },
    );
}

fn bench_decode_apply<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let wire_len = sample_wire_payload_bytes::<E>(manifest);
    let meta = CaseMetadata::from_manifest(manifest, engine, Some(wire_len));
    assert!(meta.is_complete_with_wire_sample());
    note_metadata(&meta);
    group.throughput(Throughput::Bytes(wire_len as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || setup_decode_apply::<E>(manifest),
                |(target, bytes)| {
                    measure_decode_apply(target, bytes);
                    black_box(&*target);
                },
                batch,
            );
        },
    );
}

fn bench_two_peer<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let wire_len = sample_wire_payload_bytes::<E>(manifest);
    let meta = CaseMetadata::from_manifest(manifest, engine, Some(wire_len));
    assert!(meta.is_complete_with_wire_sample());
    note_metadata(&meta);
    group.throughput(Throughput::Bytes(wire_len as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || setup_two_peer_round_trip::<E>(manifest),
                |input| black_box(measure_two_peer_round_trip(input)),
                batch,
            );
        },
    );
}

fn bench_fan_in<E: ComparisonAdapter>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    manifest: &ScenarioManifest,
    engine: EngineId,
) where
    E::StateVector: Default + Clone,
{
    let wire_len = sample_wire_payload_bytes::<E>(manifest);
    let meta = CaseMetadata::from_manifest(manifest, engine, Some(wire_len));
    assert!(meta.is_complete_with_wire_sample());
    note_metadata(&meta);
    group.throughput(Throughput::Bytes(wire_len as u64));
    let batch = criterion_batch(manifest.batch_policy);
    let m = manifest.clone();
    group.bench_with_input(
        BenchmarkId::new(function_id(manifest, engine), manifest.parameter_id.clone()),
        &m,
        |b, manifest| {
            b.iter_batched_ref(
                || setup_multi_peer_fan_in::<E>(manifest),
                |(sink, updates)| {
                    measure_multi_peer_fan_in(sink, updates);
                    black_box(&*sink);
                },
                batch,
            );
        },
    );
}

fn register_engine_cases<E: ComparisonAdapter>(
    c: &mut Criterion,
    engine: EngineId,
    group_name: &str,
) where
    E::StateVector: Default + Clone,
{
    let mut group = c.benchmark_group(group_name);
    for manifest in all_competitive_manifests() {
        match manifest.workload_id {
            w if w.starts_with("text_") => bench_public_text::<E>(&mut group, &manifest, engine),
            "integrate_decoded_update" => bench_integrate::<E>(&mut group, &manifest, engine),
            "wire_encode_full" => bench_encode_full::<E>(&mut group, &manifest, engine),
            "wire_encode_delta" => bench_encode_delta::<E>(&mut group, &manifest, engine),
            "wire_decode_apply" => bench_decode_apply::<E>(&mut group, &manifest, engine),
            "two_peer_round_trip" => bench_two_peer::<E>(&mut group, &manifest, engine),
            "multi_peer_fan_in" => bench_fan_in::<E>(&mut group, &manifest, engine),
            other => panic!("unhandled workload {other}"),
        }
    }
    group.finish();
}

/// Diagnostic-only: md-crdt Markdown structural serialize (not ratioed with Yrs).
fn diagnostic_md_crdt_markdown_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("diagnostic_md_crdt_markdown_serialize");
    for n in [1_000usize, 10_000] {
        let mut session = CollaborativeDocument::new(1);
        session.insert_paragraph(None, &fill_text(n)).expect("seed");
        group.throughput(Throughput::Bytes(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                black_box(
                    session
                        .document()
                        .serialize(EquivalenceMode::Structural)
                        .len(),
                )
            });
        });
    }
    group.finish();
}

/// Diagnostic-only: Yrs plain Text materialization (not ratioed with md-crdt).
fn diagnostic_yrs_text_get_string(c: &mut Criterion) {
    use md_crdt_yrs_bench::{TextEngine, YrsAdapter};
    let mut group = c.benchmark_group("diagnostic_yrs_text_get_string");
    for n in [1_000usize, 10_000] {
        let seed = YrsAdapter::seed(1, &fill_text(n));
        let doc = YrsAdapter::restore(&seed);
        group.throughput(Throughput::Bytes(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(doc.visible_string().len()));
        });
    }
    group.finish();
}

/// Illustrative-only: Yrs Map of N per-block Text leaves (synthetic; not competitive).
fn illustrative_yrs_block_map(c: &mut Criterion) {
    use yrs::{Doc, Map, Text, TextPrelim, Transact};
    let mut group = c.benchmark_group("illustrative_yrs_block_map");
    for n in [100usize, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || (),
                |_| {
                    let doc = Doc::with_client_id(1);
                    let map = doc.get_or_insert_map("blocks");
                    {
                        let mut txn = doc.transact_mut();
                        for i in 0..n {
                            let key = format!("b{i}");
                            let text = map.insert(&mut txn, key.as_str(), TextPrelim::new(""));
                            text.insert(&mut txn, 0, "x");
                        }
                    }
                    let txn = doc.transact();
                    black_box(map.len(&txn))
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn compare_md_crdt(c: &mut Criterion) {
    register_engine_cases::<MdCrdtAdapter>(c, EngineId::MdCrdt, "compare_md_crdt");
}

fn compare_yrs(c: &mut Criterion) {
    register_engine_cases::<YrsAdapter>(c, EngineId::Yrs, "compare_yrs");
}

/// Full suite with optional engine registration order for multi-run reports.
///
/// Set `MD_CRDT_COMPARE_ENGINE_ORDER` to `yrs_first` or `md_first` (default).
fn compare_suite(c: &mut Criterion) {
    match std::env::var("MD_CRDT_COMPARE_ENGINE_ORDER")
        .unwrap_or_else(|_| "md_first".into())
        .as_str()
    {
        "yrs_first" => {
            compare_yrs(c);
            compare_md_crdt(c);
        }
        _ => {
            compare_md_crdt(c);
            compare_yrs(c);
        }
    }
    // Non-competitive diagnostics (never ratioed across engines / models).
    diagnostic_md_crdt_markdown_serialize(c);
    diagnostic_yrs_text_get_string(c);
    illustrative_yrs_block_map(c);
}

criterion_group!(benches, compare_suite);
criterion_main!(benches);
