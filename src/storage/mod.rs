//! Persistent storage layer for CRDT documents.
//!
//! This module provides generation-based recovery using paired metadata/payload
//! slots, checksumming, and atomic file replacement. Files are synced before
//! publication; containing directories are additionally synced on Unix.

use crc32fast::Hasher;
use rkyv::{Archive, Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const SUPERBLOCK_A: &str = "superblock_a";
const SUPERBLOCK_B: &str = "superblock_b";
const SEGMENT_FILE: &str = "segment";
const SEGMENT_A: &str = "segment_a";
const SEGMENT_B: &str = "segment_b";
const OPS_DIR: &str = "ops";
const ARCHIVE_DIR: &str = "archive";
const TOMBSTONES_FILE: &str = "tombstones.bin";
const V1_VERSION: u32 = 1;
const V2_VERSION: u32 = 2;

#[derive(Debug, Archive, Serialize, Deserialize, Clone)]
struct SuperblockV1 {
    version: u32,
    seq_ref_index_flag: bool,
    pending_ops: Vec<u8>,
    segment_checksum: u32,
    segment_len: u64,
}

#[derive(Debug, Archive, Serialize, Deserialize, Clone)]
struct SuperblockV2Body {
    version: u32,
    generation: u64,
    seq_ref_index_flag: bool,
    pending_ops: Vec<u8>,
    segment_checksum: u32,
    segment_len: u64,
}

#[derive(Debug, Clone, Copy)]
struct StorageSlot {
    superblock: &'static str,
    segment: &'static str,
}

const STORAGE_SLOTS: [StorageSlot; 2] = [
    StorageSlot {
        superblock: SUPERBLOCK_A,
        segment: SEGMENT_A,
    },
    StorageSlot {
        superblock: SUPERBLOCK_B,
        segment: SEGMENT_B,
    },
];

#[derive(Debug)]
struct SuperblockMetadata {
    generation: u64,
    seq_ref_index_flag: bool,
    pending_ops: Vec<u8>,
    segment_checksum: u32,
    segment_len: u64,
    format: SuperblockFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuperblockFormat {
    V1,
    V2,
}

#[derive(Debug)]
pub struct Storage {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("corrupt storage: {0}")]
    Corrupt(&'static str),
    #[error("missing storage")]
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstoneRetention {
    KeepAll,
    MaxCount(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionReport {
    pub archived_segments: usize,
    pub archived_ops: usize,
    pub pruned_tombstones: usize,
    pub kept_tombstones: usize,
}

impl Storage {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn write_snapshot(
        &self,
        payload: &[u8],
        pending_ops: &[u8],
        seq_ref_index_flag: bool,
    ) -> Result<(), StorageError> {
        let slot_generations = [
            read_slot_generation(&self.root, STORAGE_SLOTS[0])?,
            read_slot_generation(&self.root, STORAGE_SLOTS[1])?,
        ];
        let max_generation = slot_generations.into_iter().flatten().max().unwrap_or(0);
        let target_index = slot_generations
            .iter()
            .enumerate()
            .min_by_key(|(index, generation)| (generation.unwrap_or(0), *index))
            .map(|(index, _)| index)
            .expect("storage always has two slots");
        let target = STORAGE_SLOTS[target_index];

        atomic_write_durable(&self.root, target.segment, payload)?;

        let generation = max_generation
            .checked_add(1)
            .ok_or(StorageError::Corrupt("generation overflow"))?;
        let superblock = SuperblockV2Body {
            version: V2_VERSION,
            generation,
            seq_ref_index_flag,
            pending_ops: pending_ops.to_vec(),
            segment_checksum: checksum_bytes(payload),
            segment_len: payload.len() as u64,
        };
        let body = rkyv::to_bytes::<rkyv::rancor::Error>(&superblock)
            .map_err(|_| StorageError::Corrupt("encode"))?;
        let mut encoded = Vec::with_capacity(body.len() + 4);
        encoded.extend_from_slice(&body);
        encoded.extend_from_slice(&checksum_bytes(&body).to_le_bytes());
        atomic_write_durable(&self.root, target.superblock, &encoded)?;

        Ok(())
    }

    pub fn read_snapshot(&self) -> Result<(Vec<u8>, Vec<u8>, bool), StorageError> {
        let mut candidates = Vec::new();
        let mut saw_superblock = false;
        let mut last_corruption = "decode";

        for slot in STORAGE_SLOTS {
            let bytes = match fs::read(self.root.join(slot.superblock)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(StorageError::Io(error)),
            };
            saw_superblock = true;
            match decode_superblock(&bytes) {
                Ok(metadata) => candidates.push((slot, metadata)),
                Err(StorageError::Corrupt(reason)) => last_corruption = reason,
                Err(error) => return Err(error),
            }
        }

        candidates.sort_by_key(|(_, metadata)| std::cmp::Reverse(metadata.generation));
        for (slot, metadata) in candidates {
            let segment_name = match metadata.format {
                SuperblockFormat::V1 => SEGMENT_FILE,
                SuperblockFormat::V2 => slot.segment,
            };
            let segment = match fs::read(self.root.join(segment_name)) {
                Ok(segment) => segment,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    last_corruption = "missing segment";
                    continue;
                }
                Err(error) => return Err(StorageError::Io(error)),
            };
            if segment.len() as u64 != metadata.segment_len {
                last_corruption = "length mismatch";
                continue;
            }
            if checksum_bytes(&segment) != metadata.segment_checksum {
                last_corruption = "checksum mismatch";
                continue;
            }
            return Ok((segment, metadata.pending_ops, metadata.seq_ref_index_flag));
        }

        if saw_superblock {
            Err(StorageError::Corrupt(last_corruption))
        } else {
            Err(StorageError::Missing)
        }
    }

    pub fn append_op_segment(&self, payload: &[u8]) -> Result<PathBuf, StorageError> {
        let ops_dir = self.root.join(OPS_DIR);
        fs::create_dir_all(&ops_dir)?;
        let index = next_index(&ops_dir, "op_")?;
        let path = ops_dir.join(format!("op_{index}"));
        fs::write(&path, payload)?;
        Ok(path)
    }

    pub fn compact(
        &self,
        payload: &[u8],
        pending_ops: &[u8],
        seq_ref_index_flag: bool,
        retention: TombstoneRetention,
        tombstones: &[u64],
    ) -> Result<CompactionReport, StorageError> {
        let archive_dir = self.root.join(ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir)?;

        let mut archived_segments = 0usize;
        for segment_name in [SEGMENT_FILE, SEGMENT_A, SEGMENT_B] {
            let segment_path = self.root.join(segment_name);
            if segment_path.exists() {
                let index = next_index(&archive_dir, "segment_")?;
                let archived = archive_dir.join(format!("segment_{index}"));
                fs::copy(&segment_path, &archived)?;
                archived_segments += 1;
            }
        }

        let mut archived_ops = 0usize;
        let ops_dir = self.root.join(OPS_DIR);
        if ops_dir.exists() {
            for entry in fs::read_dir(&ops_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    let index = next_index(&archive_dir, "op_")?;
                    let archived = archive_dir.join(format!("op_{index}"));
                    fs::rename(&path, &archived)?;
                    archived_ops += 1;
                }
            }
        }

        let mut all_tombstones = self.read_tombstones()?;
        all_tombstones.extend_from_slice(tombstones);
        let (kept, pruned) = prune_tombstones(all_tombstones, retention);
        let kept_tombstones = kept.len();
        let pruned_tombstones = pruned;
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&kept)
            .map_err(|_| StorageError::Corrupt("encode"))?;
        fs::write(self.root.join(TOMBSTONES_FILE), &encoded)?;

        self.write_snapshot(payload, pending_ops, seq_ref_index_flag)?;

        Ok(CompactionReport {
            archived_segments,
            archived_ops,
            pruned_tombstones,
            kept_tombstones,
        })
    }

    pub fn read_tombstones(&self) -> Result<Vec<u64>, StorageError> {
        let path = self.root.join(TOMBSTONES_FILE);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        let archived = rkyv::access::<rkyv::Archived<Vec<u64>>, rkyv::rancor::Error>(&bytes)
            .map_err(|_| StorageError::Corrupt("decode"))?;
        Ok(archived.iter().map(|v| (*v).into()).collect())
    }
}

fn read_slot_generation(root: &Path, slot: StorageSlot) -> Result<Option<u64>, StorageError> {
    match fs::read(root.join(slot.superblock)) {
        Ok(bytes) => Ok(decode_superblock(&bytes)
            .ok()
            .map(|metadata| metadata.generation)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(error)),
    }
}

fn decode_superblock(bytes: &[u8]) -> Result<SuperblockMetadata, StorageError> {
    let mut v2_version = None;
    if bytes.len() >= 4 {
        let body_len = bytes.len() - 4;
        let (body, trailer) = bytes.split_at(body_len);
        let stored_checksum = u32::from_le_bytes(
            trailer
                .try_into()
                .map_err(|_| StorageError::Corrupt("superblock checksum"))?,
        );
        if checksum_bytes(body) == stored_checksum
            && let Ok(archived) =
                rkyv::access::<ArchivedSuperblockV2Body, rkyv::rancor::Error>(body)
        {
            let version: u32 = archived.version.into();
            v2_version = Some(version);
            if version == V2_VERSION {
                return Ok(SuperblockMetadata {
                    generation: archived.generation.into(),
                    seq_ref_index_flag: archived.seq_ref_index_flag,
                    pending_ops: archived.pending_ops.to_vec(),
                    segment_checksum: archived.segment_checksum.into(),
                    segment_len: archived.segment_len.into(),
                    format: SuperblockFormat::V2,
                });
            }
        }
    }

    if let Ok(archived) = rkyv::access::<ArchivedSuperblockV1, rkyv::rancor::Error>(bytes) {
        let version: u32 = archived.version.into();
        if version != V1_VERSION {
            return Err(StorageError::Corrupt("version"));
        }
        return Ok(SuperblockMetadata {
            generation: 0,
            seq_ref_index_flag: archived.seq_ref_index_flag,
            pending_ops: archived.pending_ops.to_vec(),
            segment_checksum: archived.segment_checksum.into(),
            segment_len: archived.segment_len.into(),
            format: SuperblockFormat::V1,
        });
    }

    if v2_version.is_some() {
        Err(StorageError::Corrupt("version"))
    } else {
        Err(StorageError::Corrupt("superblock checksum"))
    }
}

fn atomic_write_durable(root: &Path, name: &str, bytes: &[u8]) -> Result<(), StorageError> {
    let path = root.join(name);
    let temp_path = root.join(format!("{name}.tmp"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, &path)?;
    sync_directory(root)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn checksum_bytes(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn next_index(dir: &Path, prefix: &str) -> Result<usize, StorageError> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut max_index: Option<usize> = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Ok(value) = rest.parse::<usize>() {
                max_index = Some(max_index.map_or(value, |current| current.max(value)));
            }
        }
    }
    Ok(max_index.map_or(0, |value| value + 1))
}

fn prune_tombstones(values: Vec<u64>, retention: TombstoneRetention) -> (Vec<u64>, usize) {
    match retention {
        TombstoneRetention::KeepAll => (values, 0),
        TombstoneRetention::MaxCount(max) => {
            if max == 0 {
                return (Vec::new(), values.len());
            }
            if values.len() <= max {
                return (values, 0);
            }
            let start = values.len() - max;
            let kept = values[start..].to_vec();
            (kept, start)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;
    use tempfile::tempdir;

    // Frozen bytes emitted by the pre-V2 Superblock layout for:
    // pending_ops="legacy-pending", payload="legacy", flag=true.
    const V1_FIXTURE: &[u8] = &[
        108, 101, 103, 97, 99, 121, 45, 112, 101, 110, 100, 105, 110, 103, 0, 0, 1, 0, 0, 0, 1, 0,
        0, 0, 232, 255, 255, 255, 14, 0, 0, 0, 238, 9, 203, 23, 0, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];

    fn read_v2_generation(path: &Path) -> u64 {
        let bytes = fs::read(path).unwrap();
        let (body, trailer) = bytes.split_at(bytes.len() - 4);
        assert_eq!(
            checksum_bytes(body),
            u32::from_le_bytes(trailer.try_into().unwrap())
        );
        let archived = rkyv::access::<ArchivedSuperblockV2Body, rkyv::rancor::Error>(body).unwrap();
        assert_eq!(archived.version, V2_VERSION);
        archived.generation.into()
    }

    fn newest_v2_slot(root: &Path) -> (&'static str, &'static str) {
        [(SUPERBLOCK_A, SEGMENT_A), (SUPERBLOCK_B, SEGMENT_B)]
            .into_iter()
            .filter(|(superblock, _)| root.join(superblock).exists())
            .max_by_key(|(superblock, _)| read_v2_generation(&root.join(superblock)))
            .unwrap()
    }

    #[test]
    fn v2_writes_alternate_slots_with_crc_and_monotonic_generations() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        storage.write_snapshot(b"one", b"p1", false).unwrap();
        assert_eq!(read_v2_generation(&dir.path().join(SUPERBLOCK_A)), 1);
        assert!(!dir.path().join(SUPERBLOCK_B).exists());

        storage.write_snapshot(b"two", b"p2", true).unwrap();
        assert_eq!(read_v2_generation(&dir.path().join(SUPERBLOCK_B)), 2);

        storage.write_snapshot(b"three", b"p3", false).unwrap();
        assert_eq!(read_v2_generation(&dir.path().join(SUPERBLOCK_A)), 3);
        assert_eq!(storage.read_snapshot().unwrap().0, b"three");
    }

    #[test]
    fn corrupt_newest_superblock_recovers_previous_generation() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"old", b"old-pending", false)
            .unwrap();
        storage
            .write_snapshot(b"new", b"new-pending", true)
            .unwrap();

        let (newest, _) = newest_v2_slot(dir.path());
        let path = dir.path().join(newest);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0xff;
        fs::write(path, bytes).unwrap();

        let (payload, pending, flag) = storage.read_snapshot().unwrap();
        assert_eq!(payload, b"old");
        assert_eq!(pending, b"old-pending");
        assert!(!flag);
    }

    #[test]
    fn write_after_corruption_repairs_bad_slot_without_clobbering_good_fallback() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"one", b"p1", false).unwrap(); // slot A, generation 1
        storage.write_snapshot(b"two", b"p2", true).unwrap(); // slot B, generation 2 (newest)

        // Corrupt the newest slot's superblock so the write path decodes it to `None`.
        let (newest, _) = newest_v2_slot(dir.path());
        let path = dir.path().join(newest);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&path, bytes).unwrap();

        // The next write must target the corrupt slot (read as generation 0), never the
        // sole surviving good slot — otherwise the only readable fallback is destroyed.
        storage.write_snapshot(b"three", b"p3", false).unwrap();

        // Good slot A is untouched at its original generation; newest read is the repair.
        assert_eq!(read_v2_generation(&dir.path().join(SUPERBLOCK_A)), 1);
        let (payload, pending, flag) = storage.read_snapshot().unwrap();
        assert_eq!(payload, b"three");
        assert_eq!(pending, b"p3");
        assert!(!flag);
    }

    #[test]
    fn crash_after_segment_replace_before_superblock_recovers_committed_generation() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"one", b"p1", false).unwrap();
        storage.write_snapshot(b"two", b"p2", true).unwrap();

        // The next write targets slot A. Replacing only its segment simulates a
        // crash after the durable segment rename but before publishing metadata.
        fs::write(dir.path().join(SEGMENT_A), b"uncommitted-three").unwrap();

        let (payload, pending, flag) = storage.read_snapshot().unwrap();
        assert_eq!(payload, b"two");
        assert_eq!(pending, b"p2");
        assert!(flag);
    }

    #[test]
    fn reads_v1_bytes_then_upgrades_without_destroying_fallback() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let payload = b"legacy";
        fs::write(dir.path().join(SEGMENT_FILE), payload).unwrap();
        fs::write(dir.path().join(SUPERBLOCK_A), V1_FIXTURE).unwrap();
        fs::write(dir.path().join(SUPERBLOCK_B), V1_FIXTURE).unwrap();

        assert_eq!(
            storage.read_snapshot().unwrap(),
            (payload.to_vec(), b"legacy-pending".to_vec(), true)
        );

        storage.write_snapshot(b"v2", b"v2-pending", false).unwrap();
        assert_eq!(storage.read_snapshot().unwrap().0, b"v2");
        assert!(dir.path().join(SEGMENT_FILE).exists());
        assert_eq!(
            [SUPERBLOCK_A, SUPERBLOCK_B]
                .into_iter()
                .filter(|name| {
                    let bytes = fs::read(dir.path().join(name)).unwrap();
                    bytes.len() >= 4
                        && checksum_bytes(&bytes[..bytes.len() - 4])
                            == u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap())
                })
                .count(),
            1
        );
    }

    #[test]
    fn test_crash_recovery_missing_superblock() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        fs::remove_file(dir.path().join(SUPERBLOCK_A)).unwrap();

        let (payload, pending, flag) = storage.read_snapshot().unwrap();
        assert_eq!(payload, b"payload");
        assert_eq!(pending, b"pending");
        assert!(!flag);
    }

    #[test]
    fn test_corruption_detection() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        let (_, segment) = newest_v2_slot(dir.path());
        let segment_path = dir.path().join(segment);
        let mut segment = fs::read(&segment_path).unwrap();
        segment[0] ^= 0xFF;
        fs::write(&segment_path, &segment).unwrap();

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::Corrupt(_) => {}
            other => panic!("Expected corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_version_mismatch() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        // Manually create superblock with wrong version
        let bad_superblock = SuperblockV1 {
            version: V1_VERSION + 1,
            seq_ref_index_flag: false,
            pending_ops: vec![],
            segment_checksum: checksum_bytes(b"payload"),
            segment_len: 7,
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&bad_superblock).unwrap();
        fs::write(dir.path().join(SUPERBLOCK_A), &encoded).unwrap();
        fs::write(dir.path().join(SUPERBLOCK_B), &encoded).unwrap();

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::Corrupt(msg) => assert_eq!(msg, "version"),
            other => panic!("Expected version corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_length_mismatch() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        // Truncate the segment file
        let (_, segment) = newest_v2_slot(dir.path());
        let segment_path = dir.path().join(segment);
        fs::write(&segment_path, b"short").unwrap();

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::Corrupt(msg) => assert_eq!(msg, "length mismatch"),
            other => panic!("Expected length mismatch error, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_storage() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::Missing => {}
            other => panic!("Expected Missing error, got {other:?}"),
        }
    }

    #[test]
    fn test_both_superblocks_missing() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        // Remove both superblocks
        for name in [SUPERBLOCK_A, SUPERBLOCK_B] {
            let path = dir.path().join(name);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::Missing => {}
            other => panic!("Expected Missing error, got {other:?}"),
        }
    }

    #[test]
    fn test_seq_ref_index_flag_preserved() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", true)
            .unwrap();

        let (_, _, flag) = storage.read_snapshot().unwrap();
        assert!(flag);
    }

    #[test]
    fn test_compact_archives_segments_and_ops() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"pending", false).unwrap();
        storage.append_op_segment(b"op1").unwrap();
        storage.append_op_segment(b"op2").unwrap();

        let report = storage
            .compact(
                b"new",
                b"pending",
                false,
                TombstoneRetention::KeepAll,
                &[1, 2],
            )
            .unwrap();

        let archive_dir = dir.path().join(ARCHIVE_DIR);
        let archived = fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .count();
        assert!(report.archived_segments >= 1);
        assert!(report.archived_ops >= 2);
        assert!(archived >= 3);

        let (payload, _, _) = storage.read_snapshot().unwrap();
        assert_eq!(payload, b"new");

        let ops_dir = dir.path().join(OPS_DIR);
        if ops_dir.exists() {
            let op_count = fs::read_dir(ops_dir).unwrap().count();
            assert_eq!(op_count, 0);
        }
    }

    #[test]
    fn test_compact_prunes_tombstones() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"payload", b"pending", false)
            .unwrap();

        let report = storage
            .compact(
                b"payload",
                b"pending",
                false,
                TombstoneRetention::MaxCount(2),
                &[1, 2, 3],
            )
            .unwrap();
        assert_eq!(report.pruned_tombstones, 1);
        assert_eq!(report.kept_tombstones, 2);
        let tombstones = storage.read_tombstones().unwrap();
        assert_eq!(tombstones, vec![2, 3]);
    }

    #[test]
    fn test_storage_overhead_targets() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let payload = vec![b'a'; 20_000];
        storage.write_snapshot(&payload, b"pending", false).unwrap();

        let active = active_storage_bytes(dir.path()).unwrap();
        let payload_len = payload.len() as u64;
        let overhead = active.saturating_sub(payload_len);
        assert!(overhead <= payload_len / 2);
    }

    #[test]
    fn test_compacted_storage_overhead_targets() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let payload = vec![b'b'; 30_000];
        storage.write_snapshot(&payload, b"pending", false).unwrap();
        storage.append_op_segment(b"op").unwrap();

        storage
            .compact(
                &payload,
                b"pending",
                false,
                TombstoneRetention::MaxCount(1),
                &[42],
            )
            .unwrap();

        let active = active_storage_bytes(dir.path()).unwrap();
        let payload_len = payload.len() as u64;
        let overhead = active.saturating_sub(payload_len);
        assert!(overhead <= payload_len + payload_len / 5);
    }

    fn active_storage_bytes(root: &Path) -> io::Result<u64> {
        let mut total = 0u64;
        for name in [
            SEGMENT_FILE,
            SEGMENT_A,
            SEGMENT_B,
            SUPERBLOCK_A,
            SUPERBLOCK_B,
            TOMBSTONES_FILE,
        ] {
            let path = root.join(name);
            if path.exists() {
                let mut file = fs::File::open(path)?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                total += buf.len() as u64;
            }
        }
        Ok(total)
    }
}
