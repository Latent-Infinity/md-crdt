use md_crdt_doc::{Block, BlockKind, Document, Parser};
use md_crdt_storage::Storage;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use tracing::debug;
use walkdir::WalkDir;

#[derive(Debug)]
pub struct Vault {
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Path does not exist: {0}")]
    PathDoesNotExist(PathBuf),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Storage error: {0}")]
    Storage(#[from] md_crdt_storage::StorageError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockFingerprint {
    pub container: Vec<String>,
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFlushedState {
    pub content_hash: u64,
    pub block_fingerprints: Vec<BlockFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    NoOp,
    Changed,
}

impl Vault {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(VaultError::PathDoesNotExist(path));
        }
        Ok(Vault { path })
    }

    pub fn files(&self) -> Vec<PathBuf> {
        WalkDir::new(&self.path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    pub fn init(&self) -> Result<(), VaultError> {
        fs::create_dir_all(self.state_root())?;
        Ok(())
    }

    pub fn flush(&self) -> Result<(), VaultError> {
        self.init()?;
        for file in self.files() {
            let content = fs::read_to_string(&file)?;
            let doc = Parser::parse(&content);
            let state = LastFlushedState {
                content_hash: hash_string(&content),
                block_fingerprints: fingerprint_document(&doc),
            };
            let encoded = bincode::serialize(&state)?;
            let storage = Storage::open(self.state_path_for(&file))?;
            storage.write_snapshot(&encoded, &[], false)?;
        }
        Ok(())
    }

    pub fn ingest(&self) -> Result<IngestResult, VaultError> {
        self.init()?;
        let mut changed = false;
        for file in self.files() {
            let content = fs::read_to_string(&file)?;
            let doc = Parser::parse(&content);
            let state = LastFlushedState {
                content_hash: hash_string(&content),
                block_fingerprints: fingerprint_document(&doc),
            };
            let storage = Storage::open(self.state_path_for(&file))?;
            match storage.read_snapshot() {
                Ok((bytes, _, _)) => {
                    let previous: LastFlushedState = bincode::deserialize(&bytes)?;
                    if previous != state {
                        changed = true;
                    }
                }
                Err(md_crdt_storage::StorageError::Missing) => {
                    changed = true;
                }
                Err(err) => return Err(VaultError::Storage(err)),
            }
        }
        if changed {
            Ok(IngestResult::Changed)
        } else {
            Ok(IngestResult::NoOp)
        }
    }

    pub fn match_blocks(
        &self,
        old: &[BlockFingerprint],
        new: &[BlockFingerprint],
    ) -> Vec<(usize, usize)> {
        match_blocks(old, new)
    }

    fn state_root(&self) -> PathBuf {
        self.path.join(".mdcrdt").join("state")
    }

    fn state_path_for(&self, file: &Path) -> PathBuf {
        let relative = file.strip_prefix(&self.path).unwrap_or(file);
        let mut path = self.state_root().join(relative);
        path.set_extension("mdcrdt");
        path
    }
}

fn fingerprint_document(doc: &Document) -> Vec<BlockFingerprint> {
    let mut fingerprints = Vec::new();
    let mut container = Vec::new();
    collect_fingerprints(&doc.blocks_in_order(), &mut container, &mut fingerprints);
    fingerprints
}

fn collect_fingerprints(
    blocks: &[&Block],
    container: &mut Vec<String>,
    out: &mut Vec<BlockFingerprint>,
) {
    for block in blocks {
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                container.push("blockquote".to_string());
                let children_blocks: Vec<_> = children.iter_asc().collect();
                collect_fingerprints(&children_blocks, container, out);
                container.pop();
            }
            other => {
                let content = block_content(other);
                out.push(BlockFingerprint {
                    container: container.clone(),
                    content_hash: hash_string(&content),
                });
            }
        }
    }
}

fn block_content(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph { text } => text.clone(),
        BlockKind::CodeFence { info, text } => format!("code:{:?}:{}", info, text),
        BlockKind::RawBlock { raw } => raw.clone(),
        BlockKind::BlockQuote { children } => {
            let rendered: Vec<String> = children
                .iter_asc()
                .map(|block| block_content(&block.kind))
                .collect();
            rendered.join("\n\n")
        }
    }
}

pub fn match_blocks(old: &[BlockFingerprint], new: &[BlockFingerprint]) -> Vec<(usize, usize)> {
    let mut candidates = Vec::new();
    for (old_idx, old_block) in old.iter().enumerate() {
        for (new_idx, new_block) in new.iter().enumerate() {
            let mut score = 0u8;
            if old_block.content_hash == new_block.content_hash {
                score += 2;
            }
            if old_block.container == new_block.container {
                score += 1;
            }
            if score > 0 {
                candidates.push((score, old_idx, new_idx));
            }
        }
    }

    candidates.sort_by(|a, b| b.cmp(a));
    let mut matched_old = HashSet::new();
    let mut matched_new = HashSet::new();
    let mut matches = Vec::new();

    for (_, old_idx, new_idx) in candidates {
        if matched_old.contains(&old_idx) || matched_new.contains(&new_idx) {
            continue;
        }
        matched_old.insert(old_idx);
        matched_new.insert(new_idx);
        matches.push((old_idx, new_idx));
    }

    let mut counts: BTreeMap<u64, usize> = BTreeMap::new();
    for block in old {
        *counts.entry(block.content_hash).or_default() += 1;
    }
    for block in new {
        if let Some(count) = counts.get(&block.content_hash)
            && *count > 1
        {
            debug!(content_hash = %format!("{:#x}", block.content_hash), "possible copy detected");
        }
    }

    matches.sort();
    matches
}

fn hash_string(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn create_mock_vault(dir: &Path) {
        fs::write(dir.join("file1.md"), "content1").unwrap();
        fs::write(dir.join("file2.md"), "content2").unwrap();
        fs::write(dir.join("not-a-markdown-file.txt"), "content3").unwrap();
        fs::create_dir(dir.join("subdir")).unwrap();
        fs::write(dir.join("subdir").join("file3.md"), "content4").unwrap();
    }

    #[test]
    fn test_vault_open_finds_markdown_files() {
        let dir = tempdir().unwrap();
        create_mock_vault(dir.path());

        let vault = Vault::open(dir.path()).unwrap();
        let mut files = vault.files();
        files.sort();

        let mut expected: Vec<_> = ["file1.md", "file2.md", "subdir/file3.md"]
            .iter()
            .map(|p| dir.path().join(p))
            .collect();
        expected.sort();

        assert_eq!(
            files, expected,
            "Vault should discover all .md files recursively"
        );
    }

    #[test]
    fn test_vault_open_errors_for_non_existent_path() {
        let dir = tempdir().unwrap();
        let non_existent_path = dir.path().join("non_existent");

        let result = Vault::open(&non_existent_path);

        assert!(result.is_err());
        if let VaultError::PathDoesNotExist(path) = result.unwrap_err() {
            assert_eq!(path, non_existent_path);
        } else {
            panic!("Expected PathDoesNotExist error");
        }
    }

    #[test]
    fn test_flush_atomic_write_and_state() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), "hello").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        let state_file = dir
            .path()
            .join(".mdcrdt")
            .join("state")
            .join("file1.mdcrdt")
            .join("segment");
        assert!(state_file.exists());

        let tmp_file = dir
            .path()
            .join(".mdcrdt")
            .join("state")
            .join("file1.mdcrdt")
            .join("segment.tmp");
        assert!(!tmp_file.exists());

        let storage = Storage::open(
            dir.path()
                .join(".mdcrdt")
                .join("state")
                .join("file1.mdcrdt"),
        )
        .unwrap();
        let (bytes, _, _) = storage.read_snapshot().unwrap();
        let state: LastFlushedState = bincode::deserialize(&bytes).unwrap();
        assert_eq!(state.content_hash, hash_string("hello"));
        assert!(!state.block_fingerprints.is_empty());
    }

    #[test]
    fn test_ingest_noop_when_unchanged() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), "hello").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        let result = vault.ingest().unwrap();
        assert_eq!(result, IngestResult::NoOp);
    }

    #[test]
    fn test_block_matching_with_container_scoring() {
        let old = vec![
            BlockFingerprint {
                container: vec!["blockquote".to_string()],
                content_hash: 1,
            },
            BlockFingerprint {
                container: vec![],
                content_hash: 2,
            },
        ];
        let new = vec![
            BlockFingerprint {
                container: vec!["blockquote".to_string()],
                content_hash: 1,
            },
            BlockFingerprint {
                container: vec![],
                content_hash: 2,
            },
        ];

        let matches = match_blocks(&old, &new);
        assert_eq!(matches, vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn test_block_matching_deterministic_greedy() {
        let old = vec![
            BlockFingerprint {
                container: vec![],
                content_hash: 1,
            },
            BlockFingerprint {
                container: vec![],
                content_hash: 1,
            },
        ];
        let new = vec![
            BlockFingerprint {
                container: vec![],
                content_hash: 1,
            },
            BlockFingerprint {
                container: vec![],
                content_hash: 1,
            },
        ];

        let matches = match_blocks(&old, &new);
        // Should match both blocks (greedy algorithm finds 2 matches)
        assert_eq!(matches.len(), 2);
        // The specific ordering may vary when scores are equal,
        // but each old and new index should appear exactly once
        let old_indices: HashSet<_> = matches.iter().map(|(o, _)| *o).collect();
        let new_indices: HashSet<_> = matches.iter().map(|(_, n)| *n).collect();
        assert_eq!(old_indices, HashSet::from([0, 1]));
        assert_eq!(new_indices, HashSet::from([0, 1]));
    }

    #[test]
    fn test_ingest_changed_when_file_modified() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), "hello").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        // Modify the file
        fs::write(dir.path().join("file1.md"), "world").unwrap();

        let result = vault.ingest().unwrap();
        assert_eq!(result, IngestResult::Changed);
    }

    #[test]
    fn test_ingest_changed_for_new_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), "hello").unwrap();
        let vault = Vault::open(dir.path()).unwrap();

        // Ingest without flush - should detect as changed
        let result = vault.ingest().unwrap();
        assert_eq!(result, IngestResult::Changed);
    }

    #[test]
    fn test_fingerprint_blockquote_document() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("file1.md"),
            "> Quote line 1\n> Quote line 2",
        )
        .unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        // Verify fingerprints were created
        let storage = Storage::open(
            dir.path()
                .join(".mdcrdt")
                .join("state")
                .join("file1.mdcrdt"),
        )
        .unwrap();
        let (bytes, _, _) = storage.read_snapshot().unwrap();
        let state: LastFlushedState = bincode::deserialize(&bytes).unwrap();

        // Should have fingerprints for blockquote content
        assert!(!state.block_fingerprints.is_empty());
        // Check that container path is set for nested content
        let has_blockquote_container = state
            .block_fingerprints
            .iter()
            .any(|fp| fp.container.contains(&"blockquote".to_string()));
        assert!(has_blockquote_container, "Should have blockquote container");
    }

    #[test]
    fn test_fingerprint_code_fence_document() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), "```rust\nfn main() {}\n```").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        let storage = Storage::open(
            dir.path()
                .join(".mdcrdt")
                .join("state")
                .join("file1.mdcrdt"),
        )
        .unwrap();
        let (bytes, _, _) = storage.read_snapshot().unwrap();
        let state: LastFlushedState = bincode::deserialize(&bytes).unwrap();

        assert!(!state.block_fingerprints.is_empty());
    }

    #[test]
    fn test_fingerprint_raw_block_document() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file1.md"), ":::custom\nraw content\n").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        vault.flush().unwrap();

        let storage = Storage::open(
            dir.path()
                .join(".mdcrdt")
                .join("state")
                .join("file1.mdcrdt"),
        )
        .unwrap();
        let (bytes, _, _) = storage.read_snapshot().unwrap();
        let state: LastFlushedState = bincode::deserialize(&bytes).unwrap();

        assert!(!state.block_fingerprints.is_empty());
    }

    #[test]
    fn test_vault_match_blocks_wrapper() {
        let vault = Vault {
            path: std::path::PathBuf::from("/tmp"),
        };
        let old = vec![BlockFingerprint {
            container: vec![],
            content_hash: 1,
        }];
        let new = vec![BlockFingerprint {
            container: vec![],
            content_hash: 1,
        }];

        let matches = vault.match_blocks(&old, &new);
        assert_eq!(matches.len(), 1);
    }
}
