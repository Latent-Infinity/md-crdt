//! Common P1 baseline/candidate workload. No production-path instrumentation.
//! Payload padding isolates returned snapshot bytes from document complexity;
//! `native` exercises the normal compressed snapshot writer as a small control.
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use md_crdt::filesync::VaultSession;
use md_crdt::session::snapshot::SessionSnapshot;
use md_crdt::{CollaborativeDocument, EquivalenceMode, Storage, StorageError, block_id_from_op};
use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};

const NOTE: &str = "note.md";
const SIZES: [usize; 4] = [0, 64 << 10, 1 << 20, 8 << 20];

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Clean,
    Active,
    Retired,
    CorruptNewest,
    TornNewest,
    MissingNewest,
    UnpublishedNewest,
}

impl Scenario {
    const ALL: [Self; 7] = [
        Self::Clean,
        Self::Active,
        Self::Retired,
        Self::CorruptNewest,
        Self::TornNewest,
        Self::MissingNewest,
        Self::UnpublishedNewest,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Active => "active",
            Self::Retired => "retired",
            Self::CorruptNewest => "corrupt_newest",
            Self::TornNewest => "torn_newest",
            Self::MissingNewest => "missing_newest",
            Self::UnpublishedNewest => "unpublished_newest",
        }
    }

    fn fallback(self) -> bool {
        matches!(
            self,
            Self::CorruptNewest | Self::TornNewest | Self::MissingNewest | Self::UnpublishedNewest
        )
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    storage: Storage,
    storage_path: PathBuf,
    selected_payload: Vec<u8>,
    replay: Vec<Vec<u8>>,
    expected: SessionSnapshot,
}

fn payload(document: &CollaborativeDocument, bytes: usize) -> Vec<u8> {
    let snapshot = document.save_snapshot().unwrap();
    if bytes == 0 {
        snapshot.to_bytes().unwrap()
    } else {
        let mut encoded = serde_json::to_vec(&snapshot).unwrap();
        assert!(
            encoded.len() <= bytes,
            "fixture JSON exceeds requested size"
        );
        encoded.resize(bytes, b' ');
        // Validate the exact bytes before any timed operation/IO marker.
        assert_eq!(SessionSnapshot::from_bytes(&encoded).unwrap(), snapshot);
        encoded
    }
}

fn append(document: &mut CollaborativeDocument, storage: &Storage, text: &str) -> Vec<u8> {
    let before = document.state_vector();
    let block = document.document().blocks_in_order()[0].id;
    document.insert_text(block, 0, text).unwrap();
    let message = document.encode_changes_since(&before).unwrap();
    let encoded = serde_json::to_vec(&message).unwrap();
    storage.append_op_segment(&encoded).unwrap();
    encoded
}

fn make_fixture(bytes: usize, scenario: Scenario) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    // Fix the persisted peer identity so separate variants use byte-identical
    // snapshot/segment inputs (vault UUID does not enter these payloads).
    fs::create_dir_all(directory.path().join(".mdcrdt")).unwrap();
    fs::write(directory.path().join(".mdcrdt/peer_id"), b"17\n").unwrap();
    let vault = VaultSession::open(directory.path()).unwrap();
    let mut document = CollaborativeDocument::new(vault.peer());
    drop(vault);
    document.insert_paragraph(None, "seed").unwrap();
    let storage_path = directory.path().join(".mdcrdt/sessions/note.mdcrdt");
    let storage = Storage::open(&storage_path).unwrap();
    let base = payload(&document, bytes);
    storage.write_snapshot(&base, &[], false).unwrap();
    let mut replay = Vec::new();
    let selected_payload;
    match scenario {
        Scenario::Clean | Scenario::Active => {
            storage.write_snapshot(&base, &[], false).unwrap();
            selected_payload = base;
            if matches!(scenario, Scenario::Active) {
                replay.push(append(&mut document, &storage, "first "));
                replay.push(append(&mut document, &storage, "second "));
            }
        }
        _ => {
            let first = append(&mut document, &storage, "retired-one ");
            let second = append(&mut document, &storage, "retired-two ");
            let covered = payload(&document, bytes);
            storage.write_snapshot(&covered, &[], false).unwrap();
            if !matches!(scenario, Scenario::UnpublishedNewest) {
                assert_eq!(storage.clear_op_segments().unwrap(), 2);
            }
            let active = append(&mut document, &storage, "active ");
            if scenario.fallback() {
                // Only two publications: the newest metadata/payload is slot B.
                // UnpublishedNewest leaves operations active: its files model
                // interruption after payload write but before metadata publish.
                // Other cases corrupt a published slot after safe retirement.
                match scenario {
                    Scenario::CorruptNewest => corrupt_same_length(&storage_path.join("segment_b")),
                    Scenario::TornNewest => {
                        fs::write(
                            storage_path.join("segment_b"),
                            &covered[..covered.len() / 2],
                        )
                        .unwrap();
                    }
                    Scenario::MissingNewest => {
                        fs::remove_file(storage_path.join("segment_b")).unwrap();
                    }
                    Scenario::UnpublishedNewest => {
                        fs::remove_file(storage_path.join("superblock_b")).unwrap();
                    }
                    _ => unreachable!("fallback scenarios handled above"),
                }
                selected_payload = base;
                replay.extend([first, second, active]);
            } else {
                selected_payload = covered;
                replay.push(active);
            }
        }
    }
    Fixture {
        directory,
        storage,
        storage_path,
        selected_payload,
        replay,
        expected: document.save_snapshot().unwrap(),
    }
}

fn corrupt_same_length(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
}

fn check_storage(fixture: &Fixture) {
    let (bytes, pending, flag) = fixture.storage.read_snapshot().unwrap();
    assert_eq!(bytes, fixture.selected_payload);
    assert!(pending.is_empty());
    assert!(!flag);
    assert_eq!(fixture.storage.read_op_segments().unwrap(), fixture.replay);
}

fn check_vault(vault: &mut VaultSession, fixture: &Fixture) {
    let document = vault.session_mut(NOTE).unwrap();
    // Whole durable state includes document, vector, delta floor, pending/deferred,
    // retained operations and next counter; render is an additional consumer check.
    assert_eq!(document.save_snapshot().unwrap(), fixture.expected);
    let expected = CollaborativeDocument::restore_from_snapshot(fixture.expected.clone()).unwrap();
    for mode in [EquivalenceMode::Structural, EquivalenceMode::Exact] {
        assert_eq!(
            document.document().serialize(mode),
            expected.document().serialize(mode)
        );
    }
    assert_eq!(document.peek_next_id(), expected.peek_next_id());
}

fn open_vault(fixture: &Fixture) -> VaultSession {
    let mut vault = VaultSession::open(fixture.directory.path()).unwrap();
    vault.session_mut(NOTE).unwrap();
    vault
}

fn label(bytes: usize, scenario: Scenario) -> String {
    let size = if bytes == 0 {
        "native".to_owned()
    } else {
        bytes.to_string()
    };
    format!("{}/{size}", scenario.name())
}

pub(super) fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_open");
    for bytes in SIZES {
        for scenario in [
            Scenario::Clean,
            Scenario::Active,
            Scenario::Retired,
            Scenario::CorruptNewest,
        ] {
            let fixture = make_fixture(bytes, scenario);
            check_storage(&fixture);
            check_vault(&mut open_vault(&fixture), &fixture);
            let label = label(bytes, scenario);
            super::allocations(&format!("storage_open/read_snapshot/{label}"), || {
                fixture.storage.read_snapshot().unwrap()
            });
            super::allocations(&format!("storage_open/read_op_segments/{label}"), || {
                fixture.storage.read_op_segments().unwrap()
            });
            super::allocations(&format!("storage_open/vault_open_and_lazy/{label}"), || {
                open_vault(&fixture)
            });
            let mut vault = VaultSession::open(fixture.directory.path()).unwrap();
            super::allocations(&format!("storage_open/vault_lazy_only/{label}"), || {
                vault.session_mut(NOTE).unwrap();
            });
            group.throughput(Throughput::Bytes(fixture.selected_payload.len() as u64));
            group.bench_function(BenchmarkId::new("read_snapshot", &label), |b| {
                b.iter(|| black_box(fixture.storage.read_snapshot().unwrap()));
            });
            group.bench_function(BenchmarkId::new("read_op_segments", &label), |b| {
                b.iter(|| black_box(fixture.storage.read_op_segments().unwrap()));
            });
            group.bench_function(BenchmarkId::new("snapshot_then_segments", &label), |b| {
                b.iter(|| {
                    black_box((
                        fixture.storage.read_snapshot().unwrap(),
                        fixture.storage.read_op_segments().unwrap(),
                    ))
                });
            });
            group.bench_function(BenchmarkId::new("vault_open_and_lazy", &label), |b| {
                b.iter_batched(
                    || (),
                    |()| black_box(open_vault(&fixture)),
                    BatchSize::PerIteration,
                );
            });
            group.bench_function(BenchmarkId::new("vault_lazy_only", &label), |b| {
                b.iter_batched(
                    || VaultSession::open(fixture.directory.path()).unwrap(),
                    |mut vault| {
                        vault.session_mut(NOTE).unwrap();
                        black_box(vault)
                    },
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

fn marker(phase: &str, edge: &str, fixture: &Fixture, label: &str) {
    // One explicit flushed line also gives strace unambiguous marker boundaries.
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    writeln!(
        stderr,
        "STORAGE_OPEN_{edge} phase={phase} case={label} root={}",
        fixture.directory.path().display()
    )
    .unwrap();
    stderr.flush().unwrap();
}

fn io_probe(bytes: usize, scenario: Scenario) {
    let fixture = make_fixture(bytes, scenario);
    let label = label(bytes, scenario);
    marker("read_snapshot", "BEGIN", &fixture, &label);
    let snapshot = fixture.storage.read_snapshot().unwrap();
    marker("read_snapshot", "END", &fixture, &label);
    assert_eq!(snapshot.0, fixture.selected_payload);
    marker("read_op_segments", "BEGIN", &fixture, &label);
    let segments = fixture.storage.read_op_segments().unwrap();
    marker("read_op_segments", "END", &fixture, &label);
    assert_eq!(segments, fixture.replay);
    marker("vault_open_and_lazy", "BEGIN", &fixture, &label);
    let mut vault = open_vault(&fixture);
    marker("vault_open_and_lazy", "END", &fixture, &label);
    check_vault(&mut vault, &fixture);
    drop(vault);
    // Fresh VaultSession, but only document lazy load is inside this boundary.
    let mut vault = VaultSession::open(fixture.directory.path()).unwrap();
    assert!(!vault.is_open(NOTE));
    marker("vault_lazy_only", "BEGIN", &fixture, &label);
    vault.session_mut(NOTE).unwrap();
    marker("vault_lazy_only", "END", &fixture, &label);
    check_vault(&mut vault, &fixture);
    println!(
        "storage_open_probe case={label} selected_bytes={} replay_segments={} replay_bytes={} result=ok",
        fixture.selected_payload.len(),
        fixture.replay.len(),
        fixture.replay.iter().map(Vec::len).sum::<usize>()
    );
}

fn flush_and_check_publication(
    vault: &mut VaultSession,
    fixture: &Fixture,
    next: md_crdt::OpId,
    require_append: bool,
) -> &'static str {
    let before = &fixture.expected.state_vector;
    let expected_delta = vault
        .session_mut(NOTE)
        .unwrap()
        .encode_changes_since(before)
        .unwrap();
    assert!(!expected_delta.ops.is_empty());
    assert!(expected_delta.ops.iter().all(|op| {
        op.id.peer == next.peer
            && op.id.counter >= next.counter
            && op.id.counter > before.get(op.id.peer).unwrap_or(0)
    }));
    let expected_after = vault.session_mut(NOTE).unwrap().save_snapshot().unwrap();
    let snapshot_before = fixture.storage.read_snapshot().unwrap().0;
    vault.flush_document(NOTE).unwrap();
    let snapshot_after = fixture.storage.read_snapshot().unwrap().0;
    let segments = fixture.storage.read_op_segments().unwrap();
    if SessionSnapshot::from_bytes(&snapshot_after).unwrap() == expected_after {
        assert!(
            !require_append,
            "existing-text delta must remain admissible"
        );
        // Auto may safely choose full publication for structural edits. Prove
        // that it covers the complete post-edit state, not merely zero segments.
        assert!(segments.is_empty());
        "full_snapshot"
    } else {
        assert_eq!(snapshot_after, snapshot_before);
        assert_eq!(segments.len(), fixture.replay.len() + 1);
        assert_eq!(&segments[..fixture.replay.len()], fixture.replay.as_slice());
        let message: md_crdt::ChangeMessage =
            serde_json::from_slice(segments.last().unwrap()).unwrap();
        // Exact message equality checks both the recovered `since` frontier and
        // the new ops; older replayed operations must never be retransmitted.
        assert_eq!(message, expected_delta);
        "append"
    }
}

fn check_existing_text_append(bytes: usize, scenario: Scenario) {
    // Independent fresh recovery keeps this check sensitive to the original
    // snapshot+replay flush frontier, not one reset by structural publication.
    let fixture = make_fixture(bytes, scenario);
    let mut vault = open_vault(&fixture);
    check_vault(&mut vault, &fixture);
    let document = vault.session_mut(NOTE).unwrap();
    let next = document.peek_next_id();
    let block = document.document().blocks_in_order()[0].id;
    // Existing block, one new unit, no new container/identity declaration:
    // delta_replay_admissible can prove this against the recovered frontier.
    document.insert_text(block, 0, "x").unwrap();
    let expected_after = vault.session_mut(NOTE).unwrap().save_snapshot().unwrap();
    let publication = flush_and_check_publication(&mut vault, &fixture, next, true);
    drop(vault);
    let mut reopened = open_vault(&fixture);
    assert_eq!(
        reopened.session_mut(NOTE).unwrap().save_snapshot().unwrap(),
        expected_after
    );
    println!(
        "storage_open_correctness case={} edit=existing_text publication={publication} result=ok",
        label(bytes, scenario)
    );
}

fn correctness() {
    for bytes in SIZES {
        for scenario in Scenario::ALL {
            let fixture = make_fixture(bytes, scenario);
            check_storage(&fixture);
            let mut vault = open_vault(&fixture);
            check_vault(&mut vault, &fixture);
            // Keep structural post-recovery editing and validate both safe Auto
            // publication outcomes; full publication is not an append failure.
            let next = vault.session_mut(NOTE).unwrap().peek_next_id();
            let block = block_id_from_op(
                vault
                    .session_mut(NOTE)
                    .unwrap()
                    .insert_paragraph(None, "after recovery")
                    .unwrap(),
            );
            assert!(
                vault
                    .session_mut(NOTE)
                    .unwrap()
                    .document()
                    .find_block_by_id(block)
                    .is_some()
            );
            let publication = flush_and_check_publication(&mut vault, &fixture, next, false);
            let expected_after = vault.session_mut(NOTE).unwrap().save_snapshot().unwrap();
            drop(vault);
            let mut reopened = open_vault(&fixture);
            assert_eq!(
                reopened.session_mut(NOTE).unwrap().save_snapshot().unwrap(),
                expected_after
            );
            println!(
                "storage_open_correctness case={} edit=structural publication={publication} result=ok",
                label(bytes, scenario)
            );
            check_existing_text_append(bytes, scenario);
        }
    }
    // Reusing the same Storage must never retain a cross-call validation cache.
    let fixture = make_fixture(64 << 10, Scenario::Retired);
    check_storage(&fixture);
    corrupt_same_length(&fixture.storage_path.join("segment_b"));
    assert_eq!(fixture.storage.read_op_segments().unwrap().len(), 3);
    assert_ne!(
        fixture.storage.read_snapshot().unwrap().0,
        fixture.selected_payload
    );
    check_vault(&mut open_vault(&fixture), &fixture);
    fs::remove_file(fixture.storage_path.join("ops/op_0")).unwrap();
    assert!(matches!(
        fixture.storage.read_op_segments(),
        Err(StorageError::CorruptOperationSegment { .. })
    ));
    assert!(
        VaultSession::open(fixture.directory.path())
            .unwrap()
            .session_mut(NOTE)
            .is_err()
    );
    let fixture = make_fixture(64 << 10, Scenario::Active);
    corrupt_same_length(&fixture.storage_path.join("ops/op_0"));
    assert!(matches!(
        fixture.storage.read_op_segments(),
        Err(StorageError::CorruptOperationSegment { .. })
    ));
    assert!(
        VaultSession::open(fixture.directory.path())
            .unwrap()
            .session_mut(NOTE)
            .is_err()
    );
    println!("storage_open_correctness case=cross_call_and_log_integrity result=ok");
}

pub(super) fn dispatch(arguments: &[String]) -> bool {
    match arguments.get(1).map(String::as_str) {
        Some("--open-io-probe") => {
            let bytes = arguments.get(2).map_or(1 << 20, |value| {
                if value == "native" {
                    0
                } else {
                    value.parse().expect("valid probe size")
                }
            });
            assert!(
                SIZES.contains(&bytes),
                "size must be native, 65536, 1048576 or 8388608"
            );
            let scenario = arguments.get(3).map_or(Scenario::Clean, |name| {
                Scenario::ALL
                    .into_iter()
                    .find(|scenario| scenario.name() == name)
                    .expect("known probe scenario")
            });
            io_probe(bytes, scenario);
            true
        }
        Some("--open-correctness") => {
            correctness();
            true
        }
        _ => false,
    }
}
