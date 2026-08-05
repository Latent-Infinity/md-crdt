//! Durability contracts for vault local edits (persist / batch / append).
//!
//! These pins exist so durability cannot silently weaken while reducing
//! snapshot rewrite cost on the edit path.

#![cfg(feature = "filesync")]

use md_crdt::doc::EquivalenceMode;
use md_crdt::filesync::VaultSession;
use md_crdt::session::CollaborativeDocument;
use md_crdt::sync::{ChangeMessage, ValidationLimits};
use md_crdt::{OpId, StateVector, block_id_from_op};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn text(vault: &mut VaultSession, path: &str) -> String {
    vault
        .session_mut(path)
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural)
}

fn first_block_id(vault: &mut VaultSession, path: &str) -> md_crdt::BlockId {
    let blocks = vault
        .session_mut(path)
        .unwrap()
        .document()
        .blocks_in_order();
    blocks.first().expect("block").id
}

fn session_dir(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.join(".mdcrdt").join("sessions").join(rel);
    path.set_extension("mdcrdt");
    path
}

fn count_op_segments(session_storage: &Path) -> usize {
    let ops = session_storage.join("ops");
    if !ops.exists() {
        return 0;
    }
    fs::read_dir(&ops)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("op_")
                .and_then(|s| s.parse::<usize>().ok())
                .is_some()
                && entry.file_type().map(|t| t.is_file()).unwrap_or(false)
        })
        .count()
}

#[test]
fn edit_is_durable_when_the_call_returns() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "hello world").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(block_id, 5, " durable").unwrap()
        })
        .unwrap();
    assert!(text(&mut vault, "note.md").contains("hello durable world"));
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert!(
        text(&mut reopened, "note.md").contains("hello durable world"),
        "acknowledged edit must survive a fresh VaultSession on the same directory"
    );
}

#[test]
fn interrupted_write_leaves_a_readable_snapshot() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "base").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .apply_local_edit("note.md", |session| {
            session.insert_text(block_id, 4, " one").unwrap()
        })
        .unwrap();
    // Two forced full snapshots populate both dual-slot generations.
    vault.compact_document("note.md").unwrap();
    let first_generation = text(&mut vault, "note.md");
    vault
        .apply_local_edit("note.md", |session| {
            session.insert_text(block_id, 4, " two").unwrap()
        })
        .unwrap();
    vault.compact_document("note.md").unwrap();
    let second_generation = text(&mut vault, "note.md");
    assert_ne!(first_generation, second_generation);
    drop(vault);

    // Simulate a torn write of the newest full-snapshot slot: replace one segment
    // body so its checksum fails. Dual-slot recovery must return the other
    // committed generation — never a half-decoded payload.
    let storage_path = session_dir(dir.path(), "note.md");
    let mut recovered_text = None;
    for name in ["segment_a", "segment_b"] {
        let path = storage_path.join(name);
        if !path.exists() {
            continue;
        }
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"torn-uncommitted-payload-not-a-snapshot").unwrap();
        match VaultSession::open(dir.path()).map(|mut v| text(&mut v, "note.md")) {
            Ok(body) => {
                assert!(
                    body == first_generation || body == second_generation,
                    "recovered text must be a prior commit, got {body:?}"
                );
                recovered_text = Some(body);
                fs::write(&path, original).unwrap();
                break;
            }
            Err(_) => {
                fs::write(&path, original).unwrap();
            }
        }
    }
    assert!(
        recovered_text.is_some(),
        "at least one torn-slot scenario must recover a readable committed generation"
    );
}

#[test]
fn unbatched_edit_still_persists_on_return() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "x").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(block_id, 1, "y").unwrap()
        })
        .unwrap();
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert_eq!(text(&mut reopened, "note.md"), "xy");
}

#[test]
fn many_edits_write_one_snapshot_when_batched() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "seed").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    // Establish a full baseline snapshot so subsequent flushes can append.
    vault.save_state("note.md").unwrap();
    let segments_before = count_op_segments(&session_dir(dir.path(), "note.md"));

    let block_id = first_block_id(&mut vault, "note.md");
    for i in 0..5 {
        vault
            .apply_local_edit("note.md", |session| {
                session
                    .insert_text(block_id, 4 + i, &format!("{i}"))
                    .unwrap()
            })
            .unwrap();
    }
    // No new segments until flush.
    assert_eq!(
        count_op_segments(&session_dir(dir.path(), "note.md")),
        segments_before
    );
    vault.flush_document("note.md").unwrap();
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    let body = text(&mut reopened, "note.md");
    assert!(body.starts_with("seed"));
    for i in 0..5 {
        assert!(body.contains(char::from(b'0' + i as u8)));
    }
}

#[test]
fn reopen_replays_appended_changes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "hello").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    vault.save_state("note.md").unwrap(); // full baseline
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .apply_local_edit("note.md", |session| {
            session.insert_text(block_id, 5, " append").unwrap()
        })
        .unwrap();
    vault.flush_document("note.md").unwrap();
    assert!(
        count_op_segments(&session_dir(dir.path(), "note.md")) >= 1,
        "expected at least one live op segment after incremental flush"
    );
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert!(text(&mut reopened, "note.md").contains("hello append"));
}

#[test]
fn full_snapshot_supersedes_its_segments() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "base").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    vault.save_state("note.md").unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    for i in 0..3 {
        vault
            .apply_local_edit("note.md", |session| {
                session.insert_text(block_id, 4, &format!("x{i}")).unwrap()
            })
            .unwrap();
        vault.flush_document("note.md").unwrap();
    }
    assert!(
        count_op_segments(&session_dir(dir.path(), "note.md")) >= 1,
        "expected live segments before compaction"
    );
    vault.compact_document("note.md").unwrap();
    let live = count_op_segments(&session_dir(dir.path(), "note.md"));
    assert_eq!(
        live, 0,
        "full snapshot should clear live op segments, found {live}"
    );
    let expected = text(&mut vault, "note.md");
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert_eq!(text(&mut reopened, "note.md"), expected);
}

#[test]
fn corrupt_trailing_segment_fails_closed() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "safe").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    vault.save_state("note.md").unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(block_id, 4, " ok").unwrap()
        })
        .unwrap();
    // Ensure there is at least one segment path to corrupt after an append cycle.
    vault
        .apply_local_edit("note.md", |session| {
            session.insert_text(block_id, 4, "!").unwrap()
        })
        .unwrap();
    vault.flush_document("note.md").unwrap();
    drop(vault);

    let ops = session_dir(dir.path(), "note.md").join("ops");
    let mut segments: Vec<_> = fs::read_dir(&ops)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("op_"))
        })
        .collect();
    segments.sort();
    let last = segments.last().expect("live op segment");
    let mut bytes = fs::read(last).unwrap();
    // Flip a payload byte while leaving the file present.
    if let Some(b) = bytes.last_mut() {
        *b ^= 0xff;
    }
    fs::write(last, bytes).unwrap();

    let result = VaultSession::open(dir.path()).and_then(|mut v| {
        v.open_document("note.md")?;
        Ok(text(&mut v, "note.md"))
    });
    assert!(
        result.is_err(),
        "corrupt trailing segment must fail closed, got {result:?}"
    );
}

#[test]
fn buffered_remote_operation_survives_flush_and_reopen() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "").unwrap();

    let mut source = CollaborativeDocument::new(77);
    source.set_unit_mode(true);
    let elem = source.insert_paragraph(None, "").unwrap();
    let block_id = block_id_from_op(elem);
    source.insert_text(block_id, 0, "abc").unwrap();
    let mut full = source
        .encode_changes_since(&StateVector::default())
        .unwrap();
    assert!(full.ops.len() >= 2, "fixture needs a causal predecessor");
    let tail = full.ops.pop().expect("later text operation");

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    let outcome = vault
        .apply_remote(
            "note.md",
            ChangeMessage {
                since: full.since.clone(),
                ops: vec![tail],
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(outcome.buffered.len(), 1, "later operation must buffer");
    drop(vault);

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    reopened
        .apply_remote("note.md", full, &ValidationLimits::default())
        .unwrap();
    assert_eq!(text(&mut reopened, "note.md"), "abc");
}

#[test]
fn deferred_edit_is_not_durable_until_flush() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    vault.save_state("note.md").unwrap();
    let block_id = first_block_id(&mut vault, "note.md");
    vault
        .apply_local_edit("note.md", |session| {
            session.insert_text(block_id, 5, " beta").unwrap()
        })
        .unwrap();
    // In-memory sees the edit...
    assert!(text(&mut vault, "note.md").contains("alpha beta"));
    drop(vault);

    // ...but without flush it must not survive reopen.
    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert_eq!(text(&mut reopened, "note.md"), "alpha");
}

/// Compile-time / API smoke: block id helper used by benches.
#[test]
fn block_id_from_op_is_stable_for_seed() {
    let id = block_id_from_op(OpId {
        counter: 1,
        peer: 1,
    });
    assert_ne!(format!("{id:?}"), "");
}
