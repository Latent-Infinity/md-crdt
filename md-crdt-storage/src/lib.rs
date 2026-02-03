use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SUPERBLOCK_A: &str = "superblock_a";
const SUPERBLOCK_B: &str = "superblock_b";
const SEGMENT_FILE: &str = "segment";
const OPS_DIR: &str = "ops";
const ARCHIVE_DIR: &str = "archive";
const TOMBSTONES_FILE: &str = "tombstones.bin";
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Superblock {
    version: u32,
    seq_ref_index_flag: bool,
    pending_ops: Vec<u8>,
    segment_checksum: u32,
    segment_len: u64,
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
        let segment_path = self.root.join(SEGMENT_FILE);
        let temp_path = self.root.join("segment.tmp");
        fs::write(&temp_path, payload)?;
        fs::rename(&temp_path, &segment_path)?;

        let checksum = checksum_bytes(payload);
        let superblock = Superblock {
            version: VERSION,
            seq_ref_index_flag,
            pending_ops: pending_ops.to_vec(),
            segment_checksum: checksum,
            segment_len: payload.len() as u64,
        };

        let encoded =
            bincode::serialize(&superblock).map_err(|_| StorageError::Corrupt("encode"))?;
        fs::write(self.root.join(SUPERBLOCK_A), &encoded)?;
        fs::write(self.root.join(SUPERBLOCK_B), &encoded)?;

        Ok(())
    }

    pub fn read_snapshot(&self) -> Result<(Vec<u8>, Vec<u8>, bool), StorageError> {
        let superblocks = [self.root.join(SUPERBLOCK_A), self.root.join(SUPERBLOCK_B)];
        let mut last_error = None;

        for path in superblocks {
            match fs::read(&path) {
                Ok(bytes) => {
                    let superblock: Superblock = bincode::deserialize(&bytes)
                        .map_err(|_| StorageError::Corrupt("decode"))?;
                    if superblock.version != VERSION {
                        return Err(StorageError::Corrupt("version"));
                    }
                    let segment_path = self.root.join(SEGMENT_FILE);
                    let segment = fs::read(&segment_path)?;
                    if segment.len() as u64 != superblock.segment_len {
                        return Err(StorageError::Corrupt("length mismatch"));
                    }
                    if checksum_bytes(&segment) != superblock.segment_checksum {
                        return Err(StorageError::Corrupt("checksum mismatch"));
                    }
                    return Ok((
                        segment,
                        superblock.pending_ops,
                        superblock.seq_ref_index_flag,
                    ));
                }
                Err(err) => last_error = Some(err),
            }
        }

        if let Some(err) = last_error {
            if err.kind() == io::ErrorKind::NotFound {
                return Err(StorageError::Missing);
            }
            return Err(StorageError::Io(err));
        }

        Err(StorageError::Missing)
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
        let segment_path = self.root.join(SEGMENT_FILE);
        if segment_path.exists() {
            let index = next_index(&archive_dir, "segment_")?;
            let archived = archive_dir.join(format!("segment_{index}"));
            fs::rename(&segment_path, &archived)?;
            archived_segments += 1;
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
        let encoded = bincode::serialize(&kept).map_err(|_| StorageError::Corrupt("encode"))?;
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
        let decoded: Vec<u64> =
            bincode::deserialize(&bytes).map_err(|_| StorageError::Corrupt("decode"))?;
        Ok(decoded)
    }
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

    #[test]
    fn test_crash_recovery_missing_superblock() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
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

        let segment_path = dir.path().join(SEGMENT_FILE);
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
        let bad_superblock = Superblock {
            version: VERSION + 1, // Wrong version
            seq_ref_index_flag: false,
            pending_ops: vec![],
            segment_checksum: checksum_bytes(b"payload"),
            segment_len: 7,
        };
        let encoded = bincode::serialize(&bad_superblock).unwrap();
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
        let segment_path = dir.path().join(SEGMENT_FILE);
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
        fs::remove_file(dir.path().join(SUPERBLOCK_A)).unwrap();
        fs::remove_file(dir.path().join(SUPERBLOCK_B)).unwrap();

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
        assert!(overhead <= payload_len / 5);
    }

    fn active_storage_bytes(root: &Path) -> io::Result<u64> {
        let mut total = 0u64;
        for name in [SEGMENT_FILE, SUPERBLOCK_A, SUPERBLOCK_B, TOMBSTONES_FILE] {
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
