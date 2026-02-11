//! File system synchronization for markdown vaults.
//!
//! This module provides vault-based file synchronization, enabling sync between
//! local markdown files and CRDT state using fingerprinting and block matching.

use crate::doc::{Block, BlockId, BlockKind, Document, Parser};
use crate::storage::Storage;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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
    Storage(#[from] crate::storage::StorageError),
    #[error("Serialization error")]
    Serialization,
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub struct Fingerprint {
    pub tokens: Vec<u64>,
    pub len: usize,
}

/// Archived version of BlockFingerprint that stores BlockId as bytes
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub struct ArchivedBlockFingerprint {
    pub block_id_bytes: [u8; 16],
    pub fingerprint: Fingerprint,
    pub container_path: Vec<usize>,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockFingerprint {
    pub block_id: BlockId,
    pub fingerprint: Fingerprint,
    pub container_path: Vec<usize>,
    pub position: usize,
}

impl From<&BlockFingerprint> for ArchivedBlockFingerprint {
    fn from(bf: &BlockFingerprint) -> Self {
        Self {
            block_id_bytes: *bf.block_id.as_bytes(),
            fingerprint: bf.fingerprint.clone(),
            container_path: bf.container_path.clone(),
            position: bf.position,
        }
    }
}

impl ArchivedBlockFingerprint {
    pub fn to_block_fingerprint(&self) -> BlockFingerprint {
        BlockFingerprint {
            block_id: BlockId::from_bytes(self.block_id_bytes),
            fingerprint: self.fingerprint.clone(),
            container_path: self.container_path.clone(),
            position: self.position,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlock {
    pub fingerprint: Fingerprint,
    pub container_path: Vec<usize>,
    pub position: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchType {
    ExactFingerprint,
    FuzzyContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockMatch {
    pub old_id: BlockId,
    pub new_id: BlockId,
    pub confidence: Score,
    pub match_type: MatchType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedBlock {
    pub id: BlockId,
    pub probable_copy_of: Option<BlockId>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BlockMapping {
    pub matched: Vec<BlockMatch>,
    pub removed: Vec<BlockId>,
    pub added: Vec<AddedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchConfig {
    pub min_match_score: Score,
    pub exact_threshold: Score,
    pub copy_threshold: Score,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            min_match_score: Score(2000),
            exact_threshold: Score(10000),
            copy_threshold: Score(7000),
        }
    }
}

/// Serializable version of LastFlushedState for rkyv
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
struct SerializableState {
    pub content_hash: u64,
    pub blocks: Vec<ArchivedBlockFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFlushedState {
    pub content_hash: u64,
    pub blocks: Vec<BlockFingerprint>,
}

impl LastFlushedState {
    fn to_serializable(&self) -> SerializableState {
        SerializableState {
            content_hash: self.content_hash,
            blocks: self
                .blocks
                .iter()
                .map(ArchivedBlockFingerprint::from)
                .collect(),
        }
    }

    fn from_archived(archived: &ArchivedSerializableState) -> Self {
        Self {
            content_hash: archived.content_hash.into(),
            blocks: archived
                .blocks
                .iter()
                .map(|b| BlockFingerprint {
                    block_id: BlockId::from_bytes(b.block_id_bytes),
                    fingerprint: Fingerprint {
                        tokens: b.fingerprint.tokens.iter().map(|&v| v.into()).collect(),
                        len: b.fingerprint.len.to_native() as usize,
                    },
                    container_path: b
                        .container_path
                        .iter()
                        .map(|&v| v.to_native() as usize)
                        .collect(),
                    position: b.position.to_native() as usize,
                })
                .collect(),
        }
    }
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

    /// Returns an iterator over markdown files in the vault.
    /// Use `.collect()` if you need a Vec.
    pub fn files(&self) -> impl Iterator<Item = PathBuf> + '_ {
        WalkDir::new(&self.path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .map(|e| e.path().to_path_buf())
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
                blocks: fingerprint_document(&doc),
            };
            let serializable = state.to_serializable();
            let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&serializable)
                .map_err(|_| VaultError::Serialization)?;
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
            let content_hash = hash_string(&content);
            let storage = Storage::open(self.state_path_for(&file))?;
            match storage.read_snapshot() {
                Ok((bytes, _, _)) => {
                    let archived =
                        rkyv::access::<ArchivedSerializableState, rkyv::rancor::Error>(&bytes)
                            .map_err(|_| VaultError::Serialization)?;
                    let previous = LastFlushedState::from_archived(archived);
                    if previous.content_hash != content_hash {
                        changed = true;
                    }
                }
                Err(crate::storage::StorageError::Missing) => {
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
        old_state: &LastFlushedState,
        new_blocks: &[ParsedBlock],
        config: &MatchConfig,
    ) -> BlockMapping {
        match_blocks(old_state, new_blocks, config)
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
    let mut container_path = Vec::new();
    collect_block_fingerprints(
        &doc.blocks_in_order(),
        &mut container_path,
        &mut fingerprints,
    );
    fingerprints
}

pub fn parsed_blocks_from_doc(doc: &Document) -> Vec<ParsedBlock> {
    let mut parsed = Vec::new();
    let mut container_path = Vec::new();
    collect_parsed_blocks(&doc.blocks_in_order(), &mut container_path, &mut parsed);
    parsed
}

fn collect_block_fingerprints(
    blocks: &[&Block],
    container_path: &mut Vec<usize>,
    out: &mut Vec<BlockFingerprint>,
) {
    for (index, block) in blocks.iter().enumerate() {
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                container_path.push(index);
                let children_blocks: Vec<_> = children.iter_asc().collect();
                collect_block_fingerprints(&children_blocks, container_path, out);
                container_path.pop();
            }
            other => {
                let content = block_content(other);
                out.push(BlockFingerprint {
                    block_id: block.id,
                    fingerprint: Fingerprint::from_content(&content),
                    container_path: container_path.clone(),
                    position: index,
                });
            }
        }
    }
}

fn collect_parsed_blocks(
    blocks: &[&Block],
    container_path: &mut Vec<usize>,
    out: &mut Vec<ParsedBlock>,
) {
    for (index, block) in blocks.iter().enumerate() {
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                container_path.push(index);
                let children_blocks: Vec<_> = children.iter_asc().collect();
                collect_parsed_blocks(&children_blocks, container_path, out);
                container_path.pop();
            }
            other => {
                let content = block_content(other);
                out.push(ParsedBlock {
                    fingerprint: Fingerprint::from_content(&content),
                    container_path: container_path.clone(),
                    position: index,
                });
            }
        }
    }
}

fn block_content(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph { text } => format!("p:{}", text),
        BlockKind::CodeFence { info, text } => format!("code:{:?}:{}", info, text),
        BlockKind::RawBlock { raw } => format!("raw:{}", raw),
        BlockKind::BlockQuote { children } => {
            let rendered: Vec<String> = children
                .iter_asc()
                .map(|block| block_content(&block.kind))
                .collect();
            format!("quote:{}", rendered.join("\n\n"))
        }
        BlockKind::Table { table } => table_fingerprint_content(table),
    }
}

fn table_fingerprint_content(table: &crate::doc::Table) -> String {
    let mut parts = Vec::new();
    parts.push("table".to_string());
    parts.push(table.header.get().join("|"));
    for row in table.rows.iter() {
        parts.push(row.cells.get().join("|"));
    }
    parts.join("\n")
}

pub fn match_blocks(
    old_state: &LastFlushedState,
    new_parsed: &[ParsedBlock],
    config: &MatchConfig,
) -> BlockMapping {
    let mut edges = Vec::new();
    for (old_idx, old) in old_state.blocks.iter().enumerate() {
        for (new_idx, new) in new_parsed.iter().enumerate() {
            if !compatible_containers(&old.container_path, &new.container_path) {
                continue;
            }
            let score = compute_score(old, new, old_idx, new_idx, old_state.blocks.len());
            if score >= config.min_match_score {
                edges.push((score, old_idx, new_idx));
            }
        }
    }

    edges.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut matched_old = HashSet::new();
    let mut matched_new = HashSet::new();
    let mut matches = Vec::new();
    for (score, old_idx, new_idx) in edges {
        if matched_old.contains(&old_idx) || matched_new.contains(&new_idx) {
            continue;
        }
        matched_old.insert(old_idx);
        matched_new.insert(new_idx);
        matches.push((old_idx, new_idx, score));
    }

    let mut result = BlockMapping::default();
    for (old_idx, _new_idx, score) in &matches {
        let old_block = &old_state.blocks[*old_idx];
        let match_type = if *score >= config.exact_threshold {
            MatchType::ExactFingerprint
        } else {
            MatchType::FuzzyContent
        };
        result.matched.push(BlockMatch {
            old_id: old_block.block_id,
            new_id: old_block.block_id,
            confidence: *score,
            match_type,
        });
    }

    for (old_idx, old) in old_state.blocks.iter().enumerate() {
        if !matched_old.contains(&old_idx) {
            result.removed.push(old.block_id);
        }
    }

    for (new_idx, new_block) in new_parsed.iter().enumerate() {
        if matched_new.contains(&new_idx) {
            continue;
        }
        let copy_source = result.matched.iter().find_map(|matched| {
            let old = old_state
                .blocks
                .iter()
                .find(|b| b.block_id == matched.old_id)?;
            if fingerprint_similarity_int(&old.fingerprint, &new_block.fingerprint)
                > config.copy_threshold.0
            {
                Some(matched.old_id)
            } else {
                None
            }
        });
        result.added.push(AddedBlock {
            id: BlockId::new_v4(),
            probable_copy_of: copy_source,
        });
    }

    result
}

fn compatible_containers(old: &[usize], new: &[usize]) -> bool {
    if old == new {
        return true;
    }
    if old.is_empty() || new.is_empty() {
        return true;
    }
    shares_prefix(old, new)
}

fn compute_score(
    old: &BlockFingerprint,
    new: &ParsedBlock,
    old_idx: usize,
    new_idx: usize,
    total: usize,
) -> Score {
    let content_sim = fingerprint_similarity_int(&old.fingerprint, &new.fingerprint);
    let dist = (old_idx as i64 - new_idx as i64).unsigned_abs() as u32;
    let total_u = total.max(1) as u32;
    let position_sim = 10000u32.saturating_sub((dist * 10000) / total_u);

    let container_score = if old.container_path == new.container_path {
        10000
    } else if shares_prefix(&old.container_path, &new.container_path) {
        5000
    } else {
        2000
    };

    let weighted = (content_sim * 60 + container_score * 25 + position_sim * 15) / 100;
    Score(weighted)
}

fn shares_prefix(a: &[usize], b: &[usize]) -> bool {
    a.len() >= 2 && b.len() >= 2 && a[..a.len() - 1] == b[..b.len() - 1]
}

fn fingerprint_similarity_int(a: &Fingerprint, b: &Fingerprint) -> u32 {
    if a.tokens.is_empty() && b.tokens.is_empty() {
        return 10000;
    }
    if a.tokens.is_empty() || b.tokens.is_empty() {
        return 0;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut intersection = 0u32;
    let mut union = 0u32;
    while i < a.tokens.len() && j < b.tokens.len() {
        match a.tokens[i].cmp(&b.tokens[j]) {
            std::cmp::Ordering::Equal => {
                intersection += 1;
                union += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                union += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                union += 1;
                j += 1;
            }
        }
    }
    union += (a.tokens.len() - i + b.tokens.len() - j) as u32;
    if union == 0 {
        0
    } else {
        (intersection * 10000) / union
    }
}

impl Fingerprint {
    pub fn from_content(content: &str) -> Self {
        let mut tokens: Vec<u64> = content
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .map(stable_hash_string)
            .collect();
        tokens.sort_unstable();
        tokens.dedup();
        Self {
            tokens,
            len: content.len(),
        }
    }
}

fn hash_string(value: &str) -> u64 {
    stable_hash_string(value)
}

fn stable_hash_string(value: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
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
        let mut files: Vec<_> = vault.files().collect();
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
        let archived =
            rkyv::access::<ArchivedSerializableState, rkyv::rancor::Error>(&bytes).unwrap();
        let state = LastFlushedState::from_archived(archived);
        assert_eq!(state.content_hash, hash_string("hello"));
        assert!(!state.blocks.is_empty());
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
        let block_a = BlockId::new_v4();
        let block_b = BlockId::new_v4();
        let old_state = LastFlushedState {
            content_hash: 0,
            blocks: vec![
                BlockFingerprint {
                    block_id: block_a,
                    fingerprint: Fingerprint::from_content("quote block"),
                    container_path: vec![0],
                    position: 0,
                },
                BlockFingerprint {
                    block_id: block_b,
                    fingerprint: Fingerprint::from_content("root block"),
                    container_path: vec![],
                    position: 1,
                },
            ],
        };
        let new_blocks = vec![
            ParsedBlock {
                fingerprint: Fingerprint::from_content("quote block"),
                container_path: vec![0],
                position: 0,
            },
            ParsedBlock {
                fingerprint: Fingerprint::from_content("root block"),
                container_path: vec![],
                position: 1,
            },
        ];

        let mapping = match_blocks(&old_state, &new_blocks, &MatchConfig::default());
        assert_eq!(mapping.matched.len(), 2);
        assert!(mapping.matched.iter().any(|m| m.old_id == block_a));
        assert!(mapping.matched.iter().any(|m| m.old_id == block_b));
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
}
