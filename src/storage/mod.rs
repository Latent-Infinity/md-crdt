//! Persistent storage layer for CRDT documents.
//!
//! This module provides generation-based recovery using paired metadata/payload
//! slots, checksumming, and atomic file replacement. Files are synced before
//! publication; containing directories are additionally synced on Unix.
//!
//! Mutating calls for a root must be externally serialized (including across
//! `Storage` handles/processes); this is not a multi-writer protocol.
//!
//! Additive V2 layout: `operations.manifest` atomically selects the active
//! operation generation and lists retired generations with their covering
//! snapshot generation and exact segment count. Generation zero uses `ops/`;
//! later generations use `ops_<generation>/`. A retirement never renames or
//! deletes active files before the manifest is durable. Retired files remain
//! replayable for the older valid snapshot slot, and may be removed only after
//! every valid slot covers them. The manifest is updated before physical
//! cleanup, so interrupted cleanup cannot create an active-log gap.
//! `coverage_a`/`coverage_b` bind each newly written V2 snapshot generation to
//! its observed operation generation/count; legacy V2 snapshots remain readable.
//!
//! Snapshot publication and retirement can share full-slot validation within one
//! externally serialized call. Validation is never retained across public calls.

use crc32fast::Hasher;
use rkyv::{Archive, Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

// Bound disk artifacts before decoding or allocating their declared collections.
fn read_artifact(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    crate::limits::read_file_bounded(path.as_ref(), 512 * 1024 * 1024)
}

const SUPERBLOCK_A: &str = "superblock_a";
const SUPERBLOCK_B: &str = "superblock_b";
const LEGACY_SEGMENT_FILE: &str = "segment";
const SEGMENT_A: &str = "segment_a";
const SEGMENT_B: &str = "segment_b";
const OPS_DIR: &str = "ops";
const ARCHIVE_DIR: &str = "archive";
const TOMBSTONES_FILE: &str = "tombstones.bin";
const OP_SEGMENT_MAGIC: &[u8; 8] = b"MDCRDTOP";
const OP_SEGMENT_VERSION: u16 = 1;
const OP_SEGMENT_HEADER_LEN: usize = 8 + 2 + 8 + 4;
const V2_VERSION: u32 = 2;
const OPERATIONS_MANIFEST: &str = "operations.manifest";
const MANIFEST_MAGIC: &[u8; 8] = b"MDCRDTGM";
const COVERAGE_MAGIC: &[u8; 8] = b"MDCRDTCV";

#[derive(Debug, Archive, Serialize, Deserialize, Clone)]
struct OperationGeneration {
    id: u64,
    count: u64,
    covered_by: u64,
}

#[derive(Debug, Archive, Serialize, Deserialize, Default)]
struct OperationManifest {
    active: u64,
    retired: Vec<OperationGeneration>,
    garbage: Vec<u64>,
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
    coverage: &'static str,
}

const STORAGE_SLOTS: [StorageSlot; 2] = [
    StorageSlot {
        superblock: SUPERBLOCK_A,
        segment: SEGMENT_A,
        coverage: "coverage_a",
    },
    StorageSlot {
        superblock: SUPERBLOCK_B,
        segment: SEGMENT_B,
        coverage: "coverage_b",
    },
];

/// Payload-validated generations for one externally serialized operation only.
struct ValidatedSlots {
    generations: [Option<u64>; 2],
}

impl ValidatedSlots {
    fn read(root: &Path) -> Result<Self, StorageError> {
        Ok(Self {
            generations: [
                read_slot_generation(root, STORAGE_SLOTS[0])?,
                read_slot_generation(root, STORAGE_SLOTS[1])?,
            ],
        })
    }

    fn newest(&self) -> Option<(StorageSlot, u64)> {
        let mut newest = None;
        for (slot, generation) in STORAGE_SLOTS.into_iter().zip(self.generations) {
            if let Some(generation) = generation
                && newest.is_none_or(|(_, current)| generation > current)
            {
                newest = Some((slot, generation));
            }
        }
        newest
    }

    fn floor(&self) -> Option<u64> {
        self.generations.into_iter().flatten().min()
    }
}

#[derive(Debug)]
struct SuperblockMetadata {
    generation: u64,
    seq_ref_index_flag: bool,
    pending_ops: Vec<u8>,
    segment_checksum: u32,
    segment_len: u64,
}

/// Validated payload and its generation, scoped to one externally serialized read.
struct SelectedSnapshot {
    payload: Vec<u8>,
    metadata: SuperblockMetadata,
}

/// Preserve the existing restore consumer's error provenance without extra reads.
#[derive(Debug)]
pub(crate) enum SnapshotAndOperationsReadError {
    Snapshot(StorageError),
    Operations(StorageError),
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
    #[error("corrupt operation segment {path}: {reason}")]
    CorruptOperationSegment { path: PathBuf, reason: &'static str },
    #[error("missing storage")]
    Missing,
    #[error(
        "storage format version {found:?} is unsupported; expected {expected}; reinitialize and re-ingest from Markdown"
    )]
    ReinitializeRequired { found: Option<u32>, expected: u32 },
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
        let mut slots = self.validated_snapshot_slots()?;
        self.write_snapshot_validated(&mut slots, payload, pending_ops, seq_ref_index_flag)
    }

    /// Publish a full snapshot and retire its covered active operation segments.
    ///
    /// Existing slot payloads are validated once for this externally serialized
    /// operation; a successfully published slot joins that validated state.
    /// Errors after publication can leave the snapshot or retirement committed.
    /// As with separate calls, fallback-required operations remain replayable and
    /// interrupted physical cleanup is restartable.
    pub fn write_snapshot_and_retire(
        &self,
        payload: &[u8],
        pending_ops: &[u8],
        seq_ref_index_flag: bool,
    ) -> Result<usize, StorageError> {
        let mut slots = self.validated_snapshot_slots()?;
        self.write_snapshot_validated(&mut slots, payload, pending_ops, seq_ref_index_flag)?;
        self.clear_op_segments_validated(&slots)
    }

    fn validated_snapshot_slots(&self) -> Result<ValidatedSlots, StorageError> {
        if self.root.join(LEGACY_SEGMENT_FILE).exists() {
            return Err(StorageError::ReinitializeRequired {
                found: None,
                expected: V2_VERSION,
            });
        }
        ValidatedSlots::read(&self.root)
    }

    fn write_snapshot_validated(
        &self,
        slots: &mut ValidatedSlots,
        payload: &[u8],
        pending_ops: &[u8],
        seq_ref_index_flag: bool,
    ) -> Result<(), StorageError> {
        let slot_generations = slots.generations;
        let max_generation = slot_generations.into_iter().flatten().max().unwrap_or(0);
        let target_index = slot_generations
            .iter()
            .enumerate()
            .min_by_key(|(index, generation)| (generation.unwrap_or(0), *index))
            .map(|(index, _)| index)
            .expect("storage always has two slots");
        let target = STORAGE_SLOTS[target_index];
        // Capture the exact generation covered by this snapshot. Retirement
        // refuses an intervening append instead of silently dropping it.
        let manifest = read_operation_manifest(&self.root)?;
        let count = operation_paths(&operation_dir(&self.root, manifest.active), None)?.len();

        let generation = max_generation
            .checked_add(1)
            .ok_or(StorageError::Corrupt("generation overflow"))?;
        let mut coverage = COVERAGE_MAGIC.to_vec();
        coverage.extend_from_slice(&generation.to_le_bytes());
        coverage.extend_from_slice(&manifest.active.to_le_bytes());
        coverage.extend_from_slice(&(count as u64).to_le_bytes());
        coverage.extend_from_slice(&checksum_bytes(&coverage).to_le_bytes());
        atomic_write_durable(&self.root, target.coverage, &coverage)?;
        #[cfg(test)]
        fault("snapshot payload")?;
        atomic_write_durable(&self.root, target.segment, payload)?;

        #[cfg(test)]
        fault("snapshot metadata")?;
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
        // The durable write published these exact bytes. Do not re-read their
        // payload within this operation, and never carry this state to a caller.
        slots.generations[target_index] = Some(generation);
        Ok(())
    }

    pub fn read_snapshot(&self) -> Result<(Vec<u8>, Vec<u8>, bool), StorageError> {
        let SelectedSnapshot { payload, metadata } = self.read_selected_snapshot()?;
        Ok((payload, metadata.pending_ops, metadata.seq_ref_index_flag))
    }

    fn read_selected_snapshot(&self) -> Result<SelectedSnapshot, StorageError> {
        let mut candidates = Vec::new();
        let mut saw_superblock = false;
        let mut last_corruption = "decode";

        for slot in STORAGE_SLOTS {
            let bytes = match read_artifact(self.root.join(slot.superblock)) {
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
            let segment = match read_artifact(self.root.join(slot.segment)) {
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
            return Ok(SelectedSnapshot {
                payload: segment,
                metadata,
            });
        }

        if self.root.join(LEGACY_SEGMENT_FILE).exists() {
            Err(StorageError::ReinitializeRequired {
                found: None,
                expected: V2_VERSION,
            })
        } else if saw_superblock {
            Err(StorageError::Corrupt(last_corruption))
        } else {
            Err(StorageError::Missing)
        }
    }

    /// Read one validated snapshot and the operations needed by that generation.
    ///
    /// The entire call must be externally serialized against mutations to this
    /// root, including through other handles/processes. Selection and its payload
    /// validation are shared only inside this call; no proof survives a return.
    /// Retired operations required by a corrupt-newest fallback remain replayable.
    pub fn read_snapshot_and_op_segments(&self) -> Result<(Vec<u8>, Vec<Vec<u8>>), StorageError> {
        self.read_snapshot_and_op_segments_staged()
            .map_err(|error| match error {
                SnapshotAndOperationsReadError::Snapshot(error)
                | SnapshotAndOperationsReadError::Operations(error) => error,
            })
    }

    pub(crate) fn read_snapshot_and_op_segments_staged(
        &self,
    ) -> Result<(Vec<u8>, Vec<Vec<u8>>), SnapshotAndOperationsReadError> {
        let selected = self
            .read_selected_snapshot()
            .map_err(SnapshotAndOperationsReadError::Snapshot)?;
        let manifest = read_operation_manifest(&self.root)
            .map_err(SnapshotAndOperationsReadError::Operations)?;
        let segments = self
            .read_op_segments_for_generation(&manifest, selected.metadata.generation)
            .map_err(SnapshotAndOperationsReadError::Operations)?;
        Ok((selected.payload, segments))
    }

    pub fn append_op_segment(&self, payload: &[u8]) -> Result<PathBuf, StorageError> {
        let manifest = read_operation_manifest(&self.root)?;
        let ops_dir = operation_dir(&self.root, manifest.active);
        if !ops_dir.exists() {
            fs::create_dir_all(&ops_dir)?;
            sync_directory(&self.root)?;
        }
        let index = next_index(&ops_dir, "op_")?;
        let name = format!("op_{index}");
        let encoded = encode_op_segment(payload);
        atomic_write_durable(&ops_dir, &name, &encoded)?;
        Ok(ops_dir.join(name))
    }

    /// Atomically retire exactly the operations covered by the latest snapshot.
    ///
    /// A successful snapshot write must precede this call, with no intervening
    /// append. Retired generations needed by the fallback slot remain replayable.
    /// Physical cleanup is independent of publication and safe to interrupt.
    pub fn clear_op_segments(&self) -> Result<usize, StorageError> {
        let slots = ValidatedSlots::read(&self.root)?;
        self.clear_op_segments_validated(&slots)
    }

    fn clear_op_segments_validated(&self, slots: &ValidatedSlots) -> Result<usize, StorageError> {
        let mut manifest = read_operation_manifest(&self.root)?;
        let paths = operation_paths(&operation_dir(&self.root, manifest.active), None)?;
        if paths.is_empty() {
            self.cleanup_retired(&mut manifest, slots)?;
            return Ok(0);
        }
        let (slot, generation) = slots.newest().ok_or(StorageError::Missing)?;
        let coverage = read_coverage(&self.root, slot, generation)?;
        if coverage != (manifest.active, paths.len() as u64) {
            return Err(StorageError::Corrupt(
                "operation log changed since snapshot",
            ));
        }
        manifest.retired.push(OperationGeneration {
            id: manifest.active,
            count: paths.len() as u64,
            covered_by: generation,
        });
        manifest.active = manifest
            .active
            .checked_add(1)
            .ok_or(StorageError::Corrupt("operation generation overflow"))?;
        #[cfg(test)]
        fault("retirement publication")?;
        write_operation_manifest(&self.root, &manifest)?;
        #[cfg(test)]
        fault("after retirement")?;
        self.cleanup_retired(&mut manifest, slots)?;
        Ok(paths.len())
    }

    fn newest_valid_slot(&self) -> Result<Option<(StorageSlot, u64)>, StorageError> {
        Ok(ValidatedSlots::read(&self.root)?.newest())
    }

    fn cleanup_retired(
        &self,
        manifest: &mut OperationManifest,
        slots: &ValidatedSlots,
    ) -> Result<(), StorageError> {
        let Some(floor) = slots.floor() else {
            return Ok(());
        };
        let removable: Vec<_> = manifest
            .retired
            .iter()
            .filter(|entry| entry.covered_by <= floor)
            .cloned()
            .collect();
        if removable.is_empty() && manifest.garbage.is_empty() {
            return Ok(());
        }
        manifest.retired.retain(|entry| entry.covered_by > floor);
        manifest
            .garbage
            .extend(removable.iter().map(|entry| entry.id));
        #[cfg(test)]
        fault("cleanup publication")?;
        write_operation_manifest(&self.root, manifest)?;
        for id in &manifest.garbage {
            let dir = operation_dir(&self.root, *id);
            if dir.exists() {
                // The manifest no longer references this directory. Partial
                // deletion, including a crash here, cannot affect replay.
                for entry in fs::read_dir(&dir)? {
                    fs::remove_file(entry?.path())?;
                    #[cfg(test)]
                    fault("cleanup deletion")?;
                }
                fs::remove_dir_all(&dir)?;
                sync_directory(&self.root)?;
            }
        }
        manifest.garbage.clear();
        #[cfg(test)]
        fault("cleanup completion")?;
        write_operation_manifest(&self.root, manifest)?;
        Ok(())
    }

    /// Whether a readable full snapshot currently exists.
    pub fn has_snapshot(&self) -> Result<bool, StorageError> {
        match self.read_snapshot() {
            Ok(_) => Ok(true),
            Err(StorageError::Missing) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Read and validate all operation segments in append order.
    pub fn read_op_segments(&self) -> Result<Vec<Vec<u8>>, StorageError> {
        let manifest = read_operation_manifest(&self.root)?;
        let snapshot_generation = self
            .newest_valid_slot()?
            .map_or(0, |(_, generation)| generation);
        self.read_op_segments_for_generation(&manifest, snapshot_generation)
    }

    fn read_op_segments_for_generation(
        &self,
        manifest: &OperationManifest,
        snapshot_generation: u64,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let mut paths = Vec::new();
        for entry in &manifest.retired {
            if entry.covered_by > snapshot_generation {
                paths.extend(operation_paths(
                    &operation_dir(&self.root, entry.id),
                    Some(entry.count),
                )?);
            }
        }
        paths.extend(operation_paths(
            &operation_dir(&self.root, manifest.active),
            None,
        )?);
        paths
            .into_iter()
            .map(|(_, path)| {
                let bytes = read_artifact(&path)?;
                decode_op_segment(&path, &bytes)
            })
            .collect()
    }

    pub fn compact(
        &self,
        payload: &[u8],
        pending_ops: &[u8],
        seq_ref_index_flag: bool,
        retention: TombstoneRetention,
        tombstones: &[u64],
    ) -> Result<CompactionReport, StorageError> {
        // Prepare all fallible input/encoding work without moving acknowledged
        // operations out of the recovery path.
        let mut all_tombstones = self.read_tombstones()?;
        all_tombstones.extend_from_slice(tombstones);
        let (kept, pruned_tombstones) = prune_tombstones(all_tombstones, retention);
        let kept_tombstones = kept.len();
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&kept)
            .map_err(|_| StorageError::Corrupt("encode"))?;
        let archive_dir = self.root.join(ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir)?;
        sync_directory(&self.root)?;
        let mut archived_segments = 0usize;
        let segment_base = next_index(&archive_dir, "segment_")?;
        for segment_name in [SEGMENT_A, SEGMENT_B] {
            let path = self.root.join(segment_name);
            if path.exists() {
                let bytes = read_artifact(path)?;
                let index = segment_base
                    .checked_add(archived_segments)
                    .ok_or(StorageError::Corrupt("archive index overflow"))?;
                atomic_write_durable(&archive_dir, &format!("segment_{index}"), &bytes)?;
                archived_segments += 1;
            }
        }
        let manifest = read_operation_manifest(&self.root)?;
        let op_paths = operation_paths(&operation_dir(&self.root, manifest.active), None)?;
        let op_base = next_index(&archive_dir, "op_")?;
        for (offset, (_, path)) in op_paths.iter().enumerate() {
            let bytes = read_artifact(path)?;
            let index = op_base
                .checked_add(offset)
                .ok_or(StorageError::Corrupt("archive index overflow"))?;
            atomic_write_durable(&archive_dir, &format!("op_{index}"), &bytes)?;
        }
        let archived_ops = op_paths.len();

        // Keep tombstone publication between the snapshot and retirement while
        // sharing this operation's validated payload state across all three.
        let mut slots = self.validated_snapshot_slots()?;
        self.write_snapshot_validated(&mut slots, payload, pending_ops, seq_ref_index_flag)?;
        atomic_write_durable(&self.root, TOMBSTONES_FILE, &encoded)?;
        self.clear_op_segments_validated(&slots)?;

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
        let bytes = read_artifact(path)?;
        let archived = rkyv::access::<rkyv::Archived<Vec<u64>>, rkyv::rancor::Error>(&bytes)
            .map_err(|_| StorageError::Corrupt("decode"))?;
        Ok(archived.iter().map(|v| (*v).into()).collect())
    }
}

fn read_slot_generation(root: &Path, slot: StorageSlot) -> Result<Option<u64>, StorageError> {
    match read_artifact(root.join(slot.superblock)) {
        Ok(bytes) => match decode_superblock(&bytes) {
            Ok(metadata) => {
                let segment = match read_artifact(root.join(slot.segment)) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                Ok((segment.len() as u64 == metadata.segment_len
                    && checksum_bytes(&segment) == metadata.segment_checksum)
                    .then_some(metadata.generation))
            }
            Err(StorageError::Corrupt(_)) => Ok(None),
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(error)),
    }
}

fn operation_dir(root: &Path, generation: u64) -> PathBuf {
    if generation == 0 {
        root.join(OPS_DIR)
    } else {
        root.join(format!("ops_{generation}"))
    }
}

fn operation_paths(
    dir: &Path,
    expected_count: Option<u64>,
) -> Result<Vec<(usize, PathBuf)>, StorageError> {
    let mut paths = Vec::new();
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(index) = name
                    .strip_prefix("op_")
                    .and_then(|v| v.parse::<usize>().ok())
                else {
                    continue;
                };
                if !entry.file_type()?.is_file() {
                    return Err(StorageError::CorruptOperationSegment {
                        path: entry.path(),
                        reason: "not a regular file",
                    });
                }
                paths.push((index, entry.path()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    paths.sort_by_key(|(index, _)| *index);
    for (expected, (actual, path)) in paths.iter().enumerate() {
        if expected != *actual {
            return Err(StorageError::CorruptOperationSegment {
                path: path.clone(),
                reason: "non-contiguous index",
            });
        }
    }
    if expected_count.is_some_and(|count| count != paths.len() as u64) {
        return Err(StorageError::CorruptOperationSegment {
            path: dir.to_path_buf(),
            reason: "retired generation count",
        });
    }
    Ok(paths)
}

fn read_operation_manifest(root: &Path) -> Result<OperationManifest, StorageError> {
    let bytes = match read_artifact(root.join(OPERATIONS_MANIFEST)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(OperationManifest::default());
        }
        Err(error) => return Err(error.into()),
    };
    let body = checked_body(&bytes, MANIFEST_MAGIC)?;
    let archived = rkyv::access::<ArchivedOperationManifest, rkyv::rancor::Error>(body)
        .map_err(|_| StorageError::Corrupt("operation manifest decode"))?;
    let manifest: OperationManifest =
        rkyv::deserialize::<OperationManifest, rkyv::rancor::Error>(archived)
            .map_err(|_| StorageError::Corrupt("operation manifest decode"))?;
    let mut previous = None;
    for entry in &manifest.retired {
        if entry.id >= manifest.active
            || entry.count == 0
            || entry.covered_by == 0
            || previous.is_some_and(|id| entry.id <= id)
        {
            return Err(StorageError::Corrupt("operation manifest ordering"));
        }
        previous = Some(entry.id);
    }
    if manifest
        .garbage
        .iter()
        .any(|id| *id >= manifest.active || manifest.retired.iter().any(|entry| entry.id == *id))
    {
        return Err(StorageError::Corrupt("operation manifest cleanup"));
    }
    Ok(manifest)
}

fn write_operation_manifest(root: &Path, manifest: &OperationManifest) -> Result<(), StorageError> {
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(manifest)
        .map_err(|_| StorageError::Corrupt("operation manifest encode"))?;
    let mut bytes = body.to_vec();
    // Keep the rkyv body at offset zero to preserve its alignment.
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&checksum_bytes(&bytes).to_le_bytes());
    atomic_write_durable(root, OPERATIONS_MANIFEST, &bytes)
}

fn checked_body<'a>(bytes: &'a [u8], magic: &[u8; 8]) -> Result<&'a [u8], StorageError> {
    if bytes.len() < 12 {
        return Err(StorageError::Corrupt("operation metadata length"));
    }
    let body_len = bytes.len() - 12;
    if &bytes[body_len..body_len + 8] != magic
        || checksum_bytes(&bytes[..bytes.len() - 4])
            != u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().expect("fixed slice"))
    {
        return Err(StorageError::Corrupt("operation metadata checksum"));
    }
    Ok(&bytes[..body_len])
}

fn read_coverage(
    root: &Path,
    slot: StorageSlot,
    generation: u64,
) -> Result<(u64, u64), StorageError> {
    let bytes = read_artifact(root.join(slot.coverage))?;
    if bytes.len() != 36
        || &bytes[..8] != COVERAGE_MAGIC
        || checksum_bytes(&bytes[..32])
            != u32::from_le_bytes(bytes[32..].try_into().expect("fixed slice"))
        || u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice")) != generation
    {
        return Err(StorageError::Corrupt("snapshot operation coverage"));
    }
    Ok((
        u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
        u64::from_le_bytes(bytes[24..32].try_into().expect("fixed slice")),
    ))
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fault(stage: &'static str) -> Result<(), StorageError> {
    FAILURE.with(|failure| {
        if failure.get() == Some(stage) {
            failure.set(None);
            Err(io::Error::other("injected storage interruption").into())
        } else {
            Ok(())
        }
    })
}

fn decode_superblock(bytes: &[u8]) -> Result<SuperblockMetadata, StorageError> {
    if bytes.len() < 4 {
        return Err(StorageError::Corrupt("superblock checksum"));
    }
    let body_len = bytes.len() - 4;
    let (body, trailer) = bytes.split_at(body_len);
    let stored_checksum = u32::from_le_bytes(
        trailer
            .try_into()
            .map_err(|_| StorageError::Corrupt("superblock checksum"))?,
    );
    if checksum_bytes(body) != stored_checksum {
        return Err(StorageError::Corrupt("superblock checksum"));
    }
    let archived = rkyv::access::<ArchivedSuperblockV2Body, rkyv::rancor::Error>(body)
        .map_err(|_| StorageError::Corrupt("decode"))?;
    let version: u32 = archived.version.into();
    if version != V2_VERSION {
        return Err(StorageError::ReinitializeRequired {
            found: Some(version),
            expected: V2_VERSION,
        });
    }
    Ok(SuperblockMetadata {
        generation: archived.generation.into(),
        seq_ref_index_flag: archived.seq_ref_index_flag,
        pending_ops: archived.pending_ops.to_vec(),
        segment_checksum: archived.segment_checksum.into(),
        segment_len: archived.segment_len.into(),
    })
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

fn encode_op_segment(payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(OP_SEGMENT_HEADER_LEN + payload.len());
    encoded.extend_from_slice(OP_SEGMENT_MAGIC);
    encoded.extend_from_slice(&OP_SEGMENT_VERSION.to_le_bytes());
    encoded.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    encoded.extend_from_slice(&checksum_bytes(payload).to_le_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn decode_op_segment(path: &Path, bytes: &[u8]) -> Result<Vec<u8>, StorageError> {
    let corrupt = |reason| StorageError::CorruptOperationSegment {
        path: path.to_path_buf(),
        reason,
    };
    if bytes.len() < OP_SEGMENT_HEADER_LEN {
        return Err(corrupt("truncated header"));
    }
    if &bytes[..8] != OP_SEGMENT_MAGIC {
        return Err(corrupt("magic"));
    }
    let version = u16::from_le_bytes(bytes[8..10].try_into().expect("fixed slice"));
    if version != OP_SEGMENT_VERSION {
        return Err(corrupt("version"));
    }
    let payload_len = u64::from_le_bytes(bytes[10..18].try_into().expect("fixed slice"));
    let payload_len = usize::try_from(payload_len).map_err(|_| corrupt("length overflow"))?;
    let expected_len = OP_SEGMENT_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| corrupt("length overflow"))?;
    if bytes.len() != expected_len {
        return Err(corrupt("length mismatch"));
    }
    let expected_checksum = u32::from_le_bytes(bytes[18..22].try_into().expect("fixed slice"));
    let payload = &bytes[OP_SEGMENT_HEADER_LEN..];
    if checksum_bytes(payload) != expected_checksum {
        return Err(corrupt("checksum mismatch"));
    }
    Ok(payload.to_vec())
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
        if let Some(rest) = name.strip_prefix(prefix)
            && let Ok(value) = rest.parse::<usize>()
        {
            max_index = Some(max_index.map_or(value, |current| current.max(value)));
        }
    }
    max_index.map_or(Ok(0), |value| {
        value
            .checked_add(1)
            .ok_or(StorageError::Corrupt("archive index overflow"))
    })
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

    fn assert_paired_matches_independent(storage: &Storage) {
        assert_eq!(
            storage.read_snapshot_and_op_segments().unwrap(),
            (
                storage.read_snapshot().unwrap().0,
                storage.read_op_segments().unwrap()
            )
        );
    }

    #[test]
    fn paired_read_requires_a_valid_snapshot() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.append_op_segment(b"active").unwrap();
        assert!(matches!(
            storage.read_snapshot_and_op_segments(),
            Err(StorageError::Missing)
        ));
        assert!(matches!(
            storage.read_snapshot_and_op_segments_staged(),
            Err(SnapshotAndOperationsReadError::Snapshot(
                StorageError::Missing
            ))
        ));
        assert_eq!(
            storage.read_op_segments().unwrap(),
            vec![b"active".to_vec()]
        );
    }

    #[test]
    fn paired_read_fences_retired_replay_and_revalidates_after_corruption() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"old", b"pending-old", false)
            .unwrap();
        let retired = storage.append_op_segment(b"retired").unwrap();
        storage
            .write_snapshot_and_retire(b"covered", b"pending-new", true)
            .unwrap();
        storage.append_op_segment(b"active").unwrap();
        assert_eq!(
            storage.read_snapshot_and_op_segments().unwrap(),
            (b"covered".to_vec(), vec![b"active".to_vec()])
        );
        assert_paired_matches_independent(&storage);
        // Same length: selecting the older generation requires payload CRC,
        // and the same handle must not retain the previously selected proof.
        fs::write(dir.path().join(SEGMENT_B), b"damaged").unwrap();
        assert_eq!(
            storage.read_snapshot_and_op_segments().unwrap(),
            (
                b"old".to_vec(),
                vec![b"retired".to_vec(), b"active".to_vec()]
            )
        );
        assert_paired_matches_independent(&storage);
        fs::remove_file(retired).unwrap();
        assert!(matches!(
            storage.read_snapshot_and_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn paired_read_validates_active_operation_payloads() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"snapshot", b"", false).unwrap();
        let active = storage.append_op_segment(b"active").unwrap();
        let mut encoded = fs::read(&active).unwrap();
        *encoded.last_mut().unwrap() ^= 1;
        fs::write(active, encoded).unwrap();
        assert!(matches!(
            storage.read_snapshot_and_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
        assert!(matches!(
            storage.read_snapshot_and_op_segments_staged(),
            Err(SnapshotAndOperationsReadError::Operations(
                StorageError::CorruptOperationSegment { .. }
            ))
        ));
    }

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
    fn corrupt_newest_payload_then_failed_publication_preserves_sole_valid_slot() {
        for combined in [false, true] {
            for stage in ["snapshot payload", "snapshot metadata"] {
                let dir = tempdir().unwrap();
                let storage = Storage::open(dir.path()).unwrap();
                storage
                    .write_snapshot(b"old", b"old-pending", false)
                    .unwrap();
                storage
                    .write_snapshot(b"new", b"new-pending", true)
                    .unwrap();
                // Keep the length unchanged so full-payload integrity matters.
                fs::write(dir.path().join(SEGMENT_B), b"bad").unwrap();
                FAILURE.with(|failure| failure.set(Some(stage)));
                let result = if combined {
                    storage
                        .write_snapshot_and_retire(b"replacement", b"next", true)
                        .map(|_| ())
                } else {
                    storage.write_snapshot(b"replacement", b"next", true)
                };
                assert!(result.is_err());
                let reopened = Storage::open(dir.path()).unwrap();
                assert_eq!(
                    reopened.read_snapshot().unwrap(),
                    (b"old".to_vec(), b"old-pending".to_vec(), false)
                );
                assert!(reopened.has_snapshot().unwrap());
            }
        }
    }

    #[test]
    fn failed_metadata_rename_after_payload_corruption_keeps_fallback_readable() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"", false).unwrap();
        storage.write_snapshot(b"new", b"", false).unwrap();
        fs::write(dir.path().join(SEGMENT_B), b"corrupt").unwrap();
        fs::create_dir(dir.path().join(format!("{SUPERBLOCK_B}.tmp"))).unwrap();
        assert!(storage.write_snapshot(b"replacement", b"", false).is_err());
        assert_eq!(
            Storage::open(dir.path())
                .unwrap()
                .read_snapshot()
                .unwrap()
                .0,
            b"old"
        );
    }

    #[test]
    fn interrupted_retirement_keeps_snapshot_and_fallback_operations_recoverable() {
        for stage in ["retirement publication", "after retirement"] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage.write_snapshot(b"old", b"", false).unwrap();
            storage.append_op_segment(b"first").unwrap();
            storage.append_op_segment(b"second").unwrap();
            storage.write_snapshot(b"covered", b"", false).unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(storage.clear_op_segments().is_err());
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"covered");
            fs::write(dir.path().join(SEGMENT_B), b"corrupt").unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"old");
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"first".to_vec(), b"second".to_vec()]
            );
        }
    }

    #[test]
    fn interrupted_cleanup_does_not_hide_active_generation_gaps() {
        for stage in [
            "cleanup publication",
            "cleanup deletion",
            "cleanup completion",
        ] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage.append_op_segment(b"first").unwrap();
            storage.append_op_segment(b"second").unwrap();
            storage.write_snapshot(b"covered", b"", false).unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(storage.clear_op_segments().is_err());
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"covered");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            // A retry completes even a partially deleted garbage generation.
            reopened.clear_op_segments().unwrap();
            let first = reopened.append_op_segment(b"active-first").unwrap();
            reopened.append_op_segment(b"active-second").unwrap();
            fs::remove_file(first).unwrap();
            assert!(matches!(
                reopened.read_op_segments(),
                Err(StorageError::CorruptOperationSegment { .. })
            ));
        }
    }

    #[test]
    fn retired_generation_is_retained_until_both_snapshot_slots_cover_it() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"", false).unwrap();
        storage.append_op_segment(b"acknowledged").unwrap();
        storage.write_snapshot(b"covered", b"", false).unwrap();
        storage.clear_op_segments().unwrap();
        assert!(storage.read_op_segments().unwrap().is_empty());
        fs::write(dir.path().join(SEGMENT_B), b"corrupt").unwrap();
        assert_eq!(storage.read_snapshot().unwrap().0, b"old");
        assert_eq!(
            storage.read_op_segments().unwrap(),
            vec![b"acknowledged".to_vec()]
        );
        storage.write_snapshot(b"recovered", b"", false).unwrap();
        storage
            .write_snapshot(b"recovered-again", b"", false)
            .unwrap();
        storage.clear_op_segments().unwrap();
        assert!(!dir.path().join(OPS_DIR).exists());
        assert!(storage.read_op_segments().unwrap().is_empty());
    }

    #[test]
    fn fallback_replay_rejects_a_missing_retired_generation_segment() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"", false).unwrap();
        let op = storage.append_op_segment(b"acknowledged").unwrap();
        storage.write_snapshot(b"covered", b"", false).unwrap();
        storage.clear_op_segments().unwrap();
        fs::remove_file(op).unwrap();
        fs::write(dir.path().join(SEGMENT_B), b"corrupt").unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn retirement_refuses_operations_appended_after_snapshot() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.append_op_segment(b"covered").unwrap();
        storage.write_snapshot(b"snapshot", b"", false).unwrap();
        storage.append_op_segment(b"not covered").unwrap();
        assert!(storage.clear_op_segments().is_err());
        assert_eq!(
            storage.read_op_segments().unwrap(),
            vec![b"covered".to_vec(), b"not covered".to_vec()]
        );
    }

    #[test]
    fn separate_retirement_revalidates_payloads_after_cross_call_corruption() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"old", b"old-pending", false)
            .unwrap();
        storage.append_op_segment(b"first").unwrap();
        storage.append_op_segment(b"second").unwrap();
        storage
            .write_snapshot(b"covered", b"new-pending", true)
            .unwrap();
        let (_, segment) = newest_v2_slot(dir.path());
        fs::write(dir.path().join(segment), b"damaged").unwrap();

        assert!(matches!(
            storage.clear_op_segments(),
            Err(StorageError::Corrupt(_))
        ));
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(
            reopened.read_snapshot().unwrap(),
            (b"old".to_vec(), b"old-pending".to_vec(), false)
        );
        assert_eq!(
            reopened.read_op_segments().unwrap(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
    }

    #[test]
    fn combined_snapshot_retires_exact_count_and_retains_fallback_replay() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"old", b"old-pending", false)
            .unwrap();
        storage.append_op_segment(b"first").unwrap();
        storage.append_op_segment(b"second").unwrap();
        assert_eq!(
            storage
                .write_snapshot_and_retire(b"covered", b"new-pending", true)
                .unwrap(),
            2
        );
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(
            reopened.read_snapshot().unwrap(),
            (b"covered".to_vec(), b"new-pending".to_vec(), true)
        );
        assert!(reopened.read_op_segments().unwrap().is_empty());

        let (_, segment) = newest_v2_slot(dir.path());
        fs::write(dir.path().join(segment), b"damaged").unwrap();
        assert_eq!(
            reopened.read_snapshot().unwrap(),
            (b"old".to_vec(), b"old-pending".to_vec(), false)
        );
        assert_eq!(
            reopened.read_op_segments().unwrap(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        assert_eq!(
            reopened
                .write_snapshot_and_retire(b"recovered", b"", false)
                .unwrap(),
            0
        );
        assert!(reopened.read_op_segments().unwrap().is_empty());
    }

    #[test]
    fn combined_publication_faults_preserve_snapshot_and_acknowledged_operations() {
        for stage in [
            "snapshot payload",
            "snapshot metadata",
            "retirement publication",
            "after retirement",
        ] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage
                .write_snapshot(b"old", b"old-pending", false)
                .unwrap();
            storage.append_op_segment(b"first").unwrap();
            storage.append_op_segment(b"second").unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .write_snapshot_and_retire(b"covered", b"new-pending", true)
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_paired_matches_independent(&reopened);
            if matches!(stage, "snapshot payload" | "snapshot metadata") {
                assert_eq!(
                    reopened.read_snapshot().unwrap(),
                    (b"old".to_vec(), b"old-pending".to_vec(), false)
                );
            } else {
                assert_eq!(
                    reopened.read_snapshot().unwrap(),
                    (b"covered".to_vec(), b"new-pending".to_vec(), true)
                );
                let (_, segment) = newest_v2_slot(dir.path());
                fs::write(dir.path().join(segment), b"damaged").unwrap();
                assert_eq!(
                    reopened.read_snapshot().unwrap(),
                    (b"old".to_vec(), b"old-pending".to_vec(), false)
                );
            }
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"first".to_vec(), b"second".to_vec()]
            );
            assert_paired_matches_independent(&reopened);
            reopened
                .write_snapshot_and_retire(b"recovered", b"", false)
                .unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"recovered");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_paired_matches_independent(&reopened);
        }
    }

    #[test]
    fn combined_cleanup_faults_preserve_both_covering_snapshots_and_allow_retry() {
        for stage in [
            "cleanup publication",
            "cleanup deletion",
            "cleanup completion",
        ] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage.write_snapshot(b"old", b"", false).unwrap();
            storage.append_op_segment(b"first").unwrap();
            storage.append_op_segment(b"second").unwrap();
            storage
                .write_snapshot_and_retire(b"covered", b"", false)
                .unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .write_snapshot_and_retire(b"covered-again", b"", false)
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"covered-again");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_paired_matches_independent(&reopened);
            let (_, segment) = newest_v2_slot(dir.path());
            fs::write(dir.path().join(segment), b"damaged-again").unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"covered");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_paired_matches_independent(&reopened);
            assert_eq!(
                reopened
                    .write_snapshot_and_retire(b"retry", b"", false)
                    .unwrap(),
                0
            );
            assert_eq!(reopened.read_snapshot().unwrap().0, b"retry");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_paired_matches_independent(&reopened);
        }
    }

    #[test]
    fn combined_retirement_manifest_io_failure_keeps_active_operations_readable() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"", false).unwrap();
        storage.append_op_segment(b"acknowledged").unwrap();
        let blocker = dir.path().join(format!("{OPERATIONS_MANIFEST}.tmp"));
        fs::create_dir(&blocker).unwrap();
        assert!(matches!(
            storage.write_snapshot_and_retire(b"covered", b"pending", true),
            Err(StorageError::Io(_))
        ));
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(
            reopened.read_snapshot().unwrap(),
            (b"covered".to_vec(), b"pending".to_vec(), true)
        );
        assert_eq!(
            reopened.read_op_segments().unwrap(),
            vec![b"acknowledged".to_vec()]
        );
        fs::remove_dir(blocker).unwrap();
        assert_eq!(
            reopened
                .write_snapshot_and_retire(b"covered", b"pending", true)
                .unwrap(),
            1
        );
        assert!(reopened.read_op_segments().unwrap().is_empty());
    }

    #[test]
    fn combined_snapshot_refuses_active_gaps_before_publication() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.write_snapshot(b"old", b"", false).unwrap();
        let first = storage.append_op_segment(b"first").unwrap();
        storage.append_op_segment(b"second").unwrap();
        fs::remove_file(first).unwrap();
        assert!(matches!(
            storage.write_snapshot_and_retire(b"replacement", b"", false),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(reopened.read_snapshot().unwrap().0, b"old");
        assert!(matches!(
            reopened.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn compaction_retirement_faults_leave_published_tombstones_and_fallback_replay() {
        for stage in ["retirement publication", "after retirement"] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage
                .compact(b"old", b"", false, TombstoneRetention::KeepAll, &[1])
                .unwrap();
            storage.append_op_segment(b"acknowledged").unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .compact(b"new", b"pending", true, TombstoneRetention::KeepAll, &[2])
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(
                reopened.read_snapshot().unwrap(),
                (b"new".to_vec(), b"pending".to_vec(), true)
            );
            assert_eq!(reopened.read_tombstones().unwrap(), vec![1, 2]);
            let (_, segment) = newest_v2_slot(dir.path());
            fs::write(dir.path().join(segment), b"bad").unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"old");
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"acknowledged".to_vec()]
            );
        }
    }

    #[test]
    fn compaction_failure_before_replacement_preserves_acknowledged_operations() {
        for stage in ["snapshot payload", "snapshot metadata"] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage.write_snapshot(b"old", b"", false).unwrap();
            storage.append_op_segment(b"acknowledged").unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .compact(b"new", b"", false, TombstoneRetention::KeepAll, &[1])
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"old");
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"acknowledged".to_vec()]
            );
            assert!(reopened.read_tombstones().unwrap().is_empty());
        }
    }

    #[test]
    fn compaction_revalidates_corrupt_payloads_before_failed_replacement() {
        for stage in ["snapshot payload", "snapshot metadata"] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage
                .write_snapshot(b"old", b"old-pending", false)
                .unwrap();
            storage.append_op_segment(b"acknowledged").unwrap();
            storage
                .write_snapshot(b"new", b"new-pending", true)
                .unwrap();
            let (_, segment) = newest_v2_slot(dir.path());
            // Equal-length corruption must be found by payload validation.
            fs::write(dir.path().join(segment), b"bad").unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .compact(
                        b"replacement",
                        b"replacement-pending",
                        true,
                        TombstoneRetention::KeepAll,
                        &[1],
                    )
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(
                reopened.read_snapshot().unwrap(),
                (b"old".to_vec(), b"old-pending".to_vec(), false)
            );
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"acknowledged".to_vec()]
            );
            assert!(reopened.read_tombstones().unwrap().is_empty());
            reopened
                .compact(b"recovered", b"", false, TombstoneRetention::KeepAll, &[1])
                .unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"recovered");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_eq!(reopened.read_tombstones().unwrap(), vec![1]);
        }
    }

    #[test]
    fn compaction_cleanup_faults_preserve_covering_snapshots_and_allow_retry() {
        for stage in [
            "cleanup publication",
            "cleanup deletion",
            "cleanup completion",
        ] {
            let dir = tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            storage
                .compact(b"old", b"", false, TombstoneRetention::KeepAll, &[1])
                .unwrap();
            storage.append_op_segment(b"first").unwrap();
            storage.append_op_segment(b"second").unwrap();
            storage
                .compact(b"covered", b"", false, TombstoneRetention::KeepAll, &[2])
                .unwrap();
            FAILURE.with(|failure| failure.set(Some(stage)));
            assert!(
                storage
                    .compact(
                        b"covered-again",
                        b"pending",
                        true,
                        TombstoneRetention::KeepAll,
                        &[3],
                    )
                    .is_err()
            );
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(
                reopened.read_snapshot().unwrap(),
                (b"covered-again".to_vec(), b"pending".to_vec(), true)
            );
            assert!(reopened.read_op_segments().unwrap().is_empty());
            assert_eq!(reopened.read_tombstones().unwrap(), vec![1, 2, 3]);
            let (_, segment) = newest_v2_slot(dir.path());
            fs::write(dir.path().join(segment), b"damaged-again").unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"covered");
            assert!(reopened.read_op_segments().unwrap().is_empty());
            reopened
                .compact(b"retry", b"", false, TombstoneRetention::KeepAll, &[])
                .unwrap();
            let reopened = Storage::open(dir.path()).unwrap();
            assert_eq!(reopened.read_snapshot().unwrap().0, b"retry");
            assert_eq!(reopened.read_tombstones().unwrap(), vec![1, 2, 3]);
            reopened.append_op_segment(b"after-cleanup").unwrap();
            assert_eq!(
                reopened.read_op_segments().unwrap(),
                vec![b"after-cleanup".to_vec()]
            );
        }
    }

    #[test]
    fn failed_tombstone_publication_preserves_prior_tombstones_and_operations() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .compact(b"old", b"", false, TombstoneRetention::KeepAll, &[1])
            .unwrap();
        storage.append_op_segment(b"acknowledged").unwrap();
        fs::create_dir(dir.path().join(format!("{TOMBSTONES_FILE}.tmp"))).unwrap();
        assert!(
            storage
                .compact(b"new", b"", false, TombstoneRetention::KeepAll, &[2])
                .is_err()
        );
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(reopened.read_snapshot().unwrap().0, b"new");
        assert_eq!(reopened.read_tombstones().unwrap(), vec![1]);
        assert_eq!(
            reopened.read_op_segments().unwrap(),
            vec![b"acknowledged".to_vec()]
        );
    }

    #[test]
    fn failed_retirement_manifest_write_keeps_active_operations_readable() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage.append_op_segment(b"acknowledged").unwrap();
        storage.write_snapshot(b"covered", b"", false).unwrap();
        fs::create_dir(dir.path().join(format!("{OPERATIONS_MANIFEST}.tmp"))).unwrap();
        assert!(storage.clear_op_segments().is_err());
        let reopened = Storage::open(dir.path()).unwrap();
        assert_eq!(reopened.read_snapshot().unwrap().0, b"covered");
        assert_eq!(
            reopened.read_op_segments().unwrap(),
            vec![b"acknowledged".to_vec()]
        );
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
    fn legacy_storage_artifact_requires_reinitialize() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        fs::write(dir.path().join(LEGACY_SEGMENT_FILE), b"legacy").unwrap();

        let error = storage.read_snapshot().unwrap_err();
        assert!(matches!(
            error,
            StorageError::ReinitializeRequired {
                found: None,
                expected: V2_VERSION,
            }
        ));
        assert!(error.to_string().contains("reinitialize and re-ingest"));
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

        // Manually create a checksummed current-layout superblock with a wrong version.
        let bad_superblock = SuperblockV2Body {
            version: V2_VERSION + 1,
            generation: 1,
            seq_ref_index_flag: false,
            pending_ops: vec![],
            segment_checksum: checksum_bytes(b"payload"),
            segment_len: 7,
        };
        let body = rkyv::to_bytes::<rkyv::rancor::Error>(&bad_superblock).unwrap();
        let mut encoded = body.to_vec();
        encoded.extend_from_slice(&checksum_bytes(&body).to_le_bytes());
        fs::write(dir.path().join(SUPERBLOCK_A), &encoded).unwrap();
        fs::write(dir.path().join(SUPERBLOCK_B), &encoded).unwrap();

        let err = storage.read_snapshot().unwrap_err();
        match err {
            StorageError::ReinitializeRequired { found, expected } => {
                assert_eq!(found, Some(V2_VERSION + 1));
                assert_eq!(expected, V2_VERSION);
            }
            other => panic!("Expected reinitialize error, got {other:?}"),
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

        assert!(storage.read_op_segments().unwrap().is_empty());
    }

    #[test]
    fn operation_segments_round_trip_in_append_order() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        storage.append_op_segment(b"first").unwrap();
        storage.append_op_segment(b"second").unwrap();
        storage
            .write_snapshot(b"covers first and second", b"", false)
            .unwrap();
        assert_eq!(storage.clear_op_segments().unwrap(), 2);
        assert!(storage.read_op_segments().unwrap().is_empty());
        // Re-seed for the remainder of the original test body.
        storage.append_op_segment(b"first").unwrap();
        storage.append_op_segment(b"second").unwrap();

        assert_eq!(
            storage.read_op_segments().unwrap(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
    }

    #[test]
    fn operation_segment_truncation_and_corruption_fail_closed() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let first = storage.append_op_segment(b"first").unwrap();
        let second = storage.append_op_segment(b"second").unwrap();

        let mut truncated = fs::read(&second).unwrap();
        truncated.pop();
        fs::write(&second, truncated).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));

        fs::remove_file(&second).unwrap();
        assert_eq!(storage.read_op_segments().unwrap(), vec![b"first".to_vec()]);

        let mut corrupt = fs::read(&first).unwrap();
        *corrupt.last_mut().unwrap() ^= 0xff;
        fs::write(first, corrupt).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn operation_segment_bad_magic_version_and_short_header_fail_closed() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let path = storage.append_op_segment(b"payload").unwrap();
        let valid = fs::read(&path).unwrap();

        // Bad magic (bytes 0..8) — caught by the magic check, not the CRC.
        let mut bad = valid.clone();
        bad[0] ^= 0xff;
        fs::write(&path, &bad).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));

        // Bad version (bytes 8..10) — sits outside the payload CRC, so it must be caught by
        // the explicit version check rather than passing as a valid segment.
        let mut bad = valid.clone();
        bad[8] = bad[8].wrapping_add(1);
        fs::write(&path, &bad).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));

        // Truncated below the header floor (< OP_SEGMENT_HEADER_LEN).
        fs::write(&path, &valid[..OP_SEGMENT_HEADER_LEN - 1]).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn operation_segments_read_in_numeric_order_and_reject_a_middle_gap() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        for index in 0..12u32 {
            storage
                .append_op_segment(format!("op-{index}").as_bytes())
                .unwrap();
        }

        // Numeric, not lexicographic: op_10/op_11 come after op_9, not after op_1.
        let expected: Vec<Vec<u8>> = (0..12).map(|i| format!("op-{i}").into_bytes()).collect();
        assert_eq!(storage.read_op_segments().unwrap(), expected);

        // A missing middle segment is a gap: reads fail closed rather than dropping ops.
        fs::remove_file(dir.path().join(OPS_DIR).join("op_5")).unwrap();
        assert!(matches!(
            storage.read_op_segments(),
            Err(StorageError::CorruptOperationSegment { .. })
        ));
    }

    #[test]
    fn failed_operation_append_preserves_prior_log_and_snapshot_generation() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .write_snapshot(b"committed", b"pending", false)
            .unwrap();
        storage.append_op_segment(b"first").unwrap();

        fs::create_dir(dir.path().join(OPS_DIR).join("op_1.tmp")).unwrap();
        assert!(matches!(
            storage.append_op_segment(b"second"),
            Err(StorageError::Io(_))
        ));

        assert_eq!(storage.read_op_segments().unwrap(), vec![b"first".to_vec()]);
        assert_eq!(storage.read_snapshot().unwrap().0, b"committed");
        assert_eq!(read_v2_generation(&dir.path().join(SUPERBLOCK_A)), 1);
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
