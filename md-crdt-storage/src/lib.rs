use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SUPERBLOCK_A: &str = "superblock_a";
const SUPERBLOCK_B: &str = "superblock_b";
const SEGMENT_FILE: &str = "segment";
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
}

fn checksum_bytes(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
}
