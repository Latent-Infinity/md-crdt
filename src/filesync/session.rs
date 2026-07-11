//! Multi-document vault session: shared peer identity + lazy CollaborativeDocuments.

use super::{
    IngestReport, LastFlushedState, MatchConfig, Vault, VaultError, fingerprint_document,
    hash_string, match_blocks, parsed_blocks_from_doc,
};
use crate::codec::JsonOpCodec;
use crate::core::{OpId, PeerId, Sequence};
use crate::doc::{Block, BlockId, BlockKind, Document, Parser, paragraph_visible_string};
use crate::session::{CollaborativeDocument, SessionError, SnapshotError};
use crate::storage::{Storage, StorageError};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Shared vault-level identity and open collaborative documents.
///
/// One peer id is stored at `.mdcrdt/peer_id` and used for every file session in
/// this vault. Documents are opened lazily into memory and persisted as
/// [`crate::session::SessionSnapshot`] blobs under `.mdcrdt/sessions/`.
pub struct VaultSession {
    pub vault: Vault,
    /// Stable peer id for this machine/vault (shared by all open docs).
    pub peer: PeerId,
    pub codec: JsonOpCodec,
    /// Lazy map: vault-relative path → session.
    docs: BTreeMap<PathBuf, CollaborativeDocument>,
}

impl VaultSession {
    /// Open a vault root, ensure `.mdcrdt` exists, and load or create peer identity.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let vault = Vault::open(path)?;
        vault.init()?;
        let peer = load_or_create_peer_id(&vault)?;
        Ok(Self {
            vault,
            peer,
            codec: JsonOpCodec,
            docs: BTreeMap::new(),
        })
    }

    /// Path of the vault-wide peer id file (`.mdcrdt/peer_id`).
    pub fn peer_id_path(vault: &Vault) -> PathBuf {
        vault.path.join(".mdcrdt").join("peer_id")
    }

    pub fn peer(&self) -> PeerId {
        self.peer
    }

    /// Vault-relative paths currently open in memory.
    pub fn open_paths(&self) -> impl Iterator<Item = &Path> {
        self.docs.keys().map(|p| p.as_path())
    }

    pub fn is_open(&self, rel_path: impl AsRef<Path>) -> bool {
        match normalize_rel(rel_path.as_ref()) {
            Ok(rel) => self.docs.contains_key(&rel),
            Err(_) => false,
        }
    }

    /// Get or lazily open a collaborative document for a vault-relative markdown path.
    ///
    /// Loads an existing session snapshot when present; otherwise creates an empty
    /// unit-mode document owned by this vault's peer.
    pub fn session_mut(
        &mut self,
        rel_path: impl AsRef<Path>,
    ) -> Result<&mut CollaborativeDocument, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        if !self.docs.contains_key(&rel) {
            let doc = self.load_or_create_session(&rel)?;
            self.docs.insert(rel.clone(), doc);
        }
        Ok(self.docs.get_mut(&rel).expect("session inserted above"))
    }

    /// Persist one open document's session snapshot to storage.
    pub fn save(&self, rel_path: impl AsRef<Path>) -> Result<(), VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let doc = self
            .docs
            .get(&rel)
            .ok_or_else(|| VaultError::SessionNotOpen(rel.clone()))?;
        write_session_snapshot(&self.vault, &rel, doc)
    }

    /// Persist all open document snapshots.
    pub fn save_all(&self) -> Result<(), VaultError> {
        for rel in self.docs.keys() {
            let doc = self.docs.get(rel).expect("key from map");
            write_session_snapshot(&self.vault, rel, doc)?;
        }
        Ok(())
    }

    /// Alias for [`Self::save_all`] — snapshot flush, not markdown export.
    pub fn flush_all(&self) -> Result<(), VaultError> {
        self.save_all()
    }

    /// Drop an in-memory session without saving (disk snapshot left unchanged).
    pub fn close(&mut self, rel_path: impl AsRef<Path>) -> Result<(), VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.docs.remove(&rel);
        Ok(())
    }

    /// Structure-only ingest of all markdown files (hash gate → match_blocks → block ops).
    ///
    /// Text LCS for matched-but-edited paragraphs is deferred. New paragraphs use N6-d
    /// (`insert_paragraph` = empty InsertBlock + InsertText).
    pub fn ingest_all(&mut self) -> Result<IngestReport, VaultError> {
        let files: Vec<PathBuf> = self.vault.files().collect();
        let mut report = IngestReport::default();
        for abs in files {
            let rel = abs
                .strip_prefix(&self.vault.path)
                .unwrap_or(abs.as_path())
                .to_path_buf();
            match self.ingest_file(&rel)? {
                IngestOutcome::Changed(ops) => {
                    report.files_changed += 1;
                    report.ops_emitted += ops;
                }
                IngestOutcome::NoOp => report.files_noop += 1,
                IngestOutcome::Skipped => report.files_skipped += 1,
            }
        }
        Ok(report)
    }

    /// Structure ingest for a single vault-relative markdown path.
    ///
    /// Returns `(changed, ops_emitted)`.
    pub fn ingest_file(&mut self, rel_path: impl AsRef<Path>) -> Result<IngestOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let abs = self.vault.path.join(&rel);
        if !abs.exists() {
            return Err(VaultError::PathDoesNotExist(abs));
        }
        let content = fs::read_to_string(&abs)?;
        let content_hash = hash_string(&content);

        if let Some(prev) = self.vault.read_last_flushed(&abs)?
            && prev.content_hash == content_hash
        {
            return Ok(IngestOutcome::NoOp);
        }

        let parsed = Parser::parse(&content);
        // Tables aren't wire-ready yet: skip the whole file (don't record its hash, so it
        // gets picked up once table support lands). No silent flatten, no vault-wide abort.
        if document_contains_table(&parsed) {
            return Ok(IngestOutcome::Skipped);
        }

        // Ensure session is loaded before matching against its CRDT state.
        self.session_mut(&rel)?;
        let empty = self
            .docs
            .get(&rel)
            .expect("session opened above")
            .document()
            .blocks_in_order()
            .is_empty();

        // Nested re-ingest matching (blockquote edits preserving identity) is a follow-up;
        // skip a blockquote file when the session already has content.
        if !empty && document_contains_blockquote(&parsed) {
            return Ok(IngestOutcome::Skipped);
        }

        let ops = {
            let session = self.docs.get_mut(&rel).expect("session opened above");
            if empty {
                // First ingest: insert the parsed tree recursively (blockquotes preserved).
                let blocks = parsed.blocks_in_order();
                insert_tree(session, None, &blocks)?
            } else {
                // Re-ingest of a flat document: structure match against CRDT state.
                let leaves = leaf_blocks_in_order(&parsed);
                let new_parsed = parsed_blocks_from_doc(&parsed);
                apply_structure_ingest(session, &leaves, &new_parsed)?
            }
        };

        // Persist session snapshot + fingerprint/hash gate state.
        self.save(&rel)?;
        let session = self.docs.get(&rel).expect("session still open");
        let state = LastFlushedState {
            content_hash,
            blocks: fingerprint_document(session.document()),
        };
        self.vault.write_last_flushed(&abs, &state)?;
        Ok(IngestOutcome::Changed(ops))
    }

    fn load_or_create_session(&self, rel: &Path) -> Result<CollaborativeDocument, VaultError> {
        let storage_path = session_storage_path(&self.vault, rel);
        match Storage::open(&storage_path) {
            Ok(storage) => match CollaborativeDocument::read_from_storage(&storage) {
                Ok(mut doc) => {
                    // Vault peer is authoritative for local edits across all files.
                    if doc.peer() != self.peer {
                        doc.rebind_peer(self.peer);
                    }
                    // Ensure unit-mode for collaborative vault sessions.
                    if !doc.unit_mode() {
                        doc.set_unit_mode(true);
                    }
                    Ok(doc)
                }
                Err(SnapshotError::Storage(StorageError::Missing)) => {
                    Ok(CollaborativeDocument::new(self.peer))
                }
                Err(err) => Err(VaultError::Snapshot(err.to_string())),
            },
            Err(StorageError::Missing) => {
                // Storage::open creates dirs; Missing is rare — treat as empty.
                Ok(CollaborativeDocument::new(self.peer))
            }
            Err(err) => Err(VaultError::Storage(err)),
        }
    }
}

fn load_or_create_peer_id(vault: &Vault) -> Result<PeerId, VaultError> {
    let path = VaultSession::peer_id_path(vault);
    if path.exists() {
        let raw = fs::read_to_string(&path)?;
        let trimmed = raw.trim();
        let peer: PeerId = trimmed
            .parse()
            .map_err(|_| VaultError::InvalidPeerId(trimmed.to_string()))?;
        if peer == 0 {
            return Err(VaultError::InvalidPeerId(trimmed.to_string()));
        }
        return Ok(peer);
    }

    // First open: allocate a non-zero peer and persist it for this vault.
    let peer = generate_peer_id(vault);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, format!("{peer}\n"))?;
    Ok(peer)
}

fn generate_peer_id(vault: &Vault) -> PeerId {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut hasher = DefaultHasher::new();
    vault.path.hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    // Avoid 0: sync rejects counter 0; peer 0 is reserved for parse-seeded units.
    let mut peer = hasher.finish();
    if peer == 0 {
        peer = 1;
    }
    peer
}

/// Storage directory for collaborative session snapshots (separate from fingerprint state).
fn sessions_root(vault: &Vault) -> PathBuf {
    vault.path.join(".mdcrdt").join("sessions")
}

fn session_storage_path(vault: &Vault, rel: &Path) -> PathBuf {
    let mut path = sessions_root(vault).join(rel);
    path.set_extension("mdcrdt");
    path
}

fn write_session_snapshot(
    vault: &Vault,
    rel: &Path,
    doc: &CollaborativeDocument,
) -> Result<(), VaultError> {
    let storage_path = session_storage_path(vault, rel);
    if let Some(parent) = storage_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let storage = Storage::open(&storage_path)?;
    doc.write_to_storage(&storage)
        .map_err(|e| VaultError::Snapshot(e.to_string()))?;
    Ok(())
}

/// Leaf blocks in the same traversal order as [`parsed_blocks_from_doc`].
fn leaf_blocks_in_order(doc: &Document) -> Vec<Block> {
    let mut out = Vec::new();
    collect_leaf_blocks(&doc.blocks_in_order(), &mut out);
    out
}

fn collect_leaf_blocks(blocks: &[&Block], out: &mut Vec<Block>) {
    for block in blocks {
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                let kids: Vec<_> = children.iter_asc().collect();
                collect_leaf_blocks(&kids, out);
            }
            _ => out.push((*block).clone()),
        }
    }
}

fn find_elem_id(session: &CollaborativeDocument, block_id: BlockId) -> Option<OpId> {
    session
        .document()
        .blocks_in_order()
        .into_iter()
        .find(|b| b.id == block_id)
        .map(|b| b.elem_id)
}

fn session_err(err: SessionError) -> VaultError {
    VaultError::Session(err.to_string())
}

/// Outcome of ingesting a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// File unchanged since last flush (hash gate).
    NoOp,
    /// File ingested; approximate structure-op count.
    Changed(usize),
    /// File contains not-yet-ingestable blocks (a table, or a blockquote on re-ingest).
    Skipped,
}

fn document_contains_table(doc: &Document) -> bool {
    fn walk(blocks: &[&Block]) -> bool {
        blocks.iter().any(|b| match &b.kind {
            BlockKind::Table { .. } => true,
            BlockKind::BlockQuote { children } => walk(&children.iter_asc().collect::<Vec<_>>()),
            _ => false,
        })
    }
    walk(&doc.blocks_in_order())
}

fn document_contains_blockquote(doc: &Document) -> bool {
    doc.blocks_in_order()
        .iter()
        .any(|b| matches!(b.kind, BlockKind::BlockQuote { .. }))
}

/// Insert a parsed block tree into `parent`'s children (top-level when `None`),
/// preserving blockquote nesting. Returns an approximate structure-op count.
fn insert_tree(
    session: &mut CollaborativeDocument,
    parent: Option<OpId>,
    blocks: &[&Block],
) -> Result<usize, VaultError> {
    let mut ops = 0usize;
    let mut after: Option<OpId> = None;
    for block in blocks {
        let elem = match &block.kind {
            BlockKind::Paragraph { text } => {
                let body = paragraph_visible_string(text);
                ops += if body.is_empty() { 1 } else { 2 };
                session
                    .insert_paragraph_in(parent, after, &body)
                    .map_err(session_err)?
            }
            BlockKind::CodeFence { info, text } => {
                ops += 1;
                session
                    .insert_block_in(
                        parent,
                        after,
                        BlockKind::CodeFence {
                            info: info.clone(),
                            text: text.clone(),
                        },
                    )
                    .map_err(session_err)?
            }
            BlockKind::RawBlock { raw } => {
                ops += 1;
                session
                    .insert_block_in(parent, after, BlockKind::RawBlock { raw: raw.clone() })
                    .map_err(session_err)?
            }
            BlockKind::BlockQuote { children } => {
                ops += 1;
                let q = session
                    .insert_block_in(
                        parent,
                        after,
                        BlockKind::BlockQuote {
                            children: Sequence::new(),
                        },
                    )
                    .map_err(session_err)?;
                let kids: Vec<_> = children.iter_asc().collect();
                ops += insert_tree(session, Some(q), &kids)?;
                q
            }
            BlockKind::Table { .. } => return Err(VaultError::UnsupportedIngestBlock("table")),
        };
        after = Some(elem);
    }
    Ok(ops)
}

/// Structure-only: delete removed blocks, insert added (N6-d for paragraphs).
///
/// Matched blocks keep their CRDT identity; text edits on matched blocks are left
/// for a later text-diff path.
fn apply_structure_ingest(
    session: &mut CollaborativeDocument,
    leaves: &[Block],
    new_parsed: &[super::ParsedBlock],
) -> Result<usize, VaultError> {
    let old_state = LastFlushedState {
        content_hash: 0,
        blocks: fingerprint_document(session.document()),
    };
    let mapping = match_blocks(&old_state, new_parsed, &MatchConfig::default());

    let mut ops = 0usize;

    // Deletes first so insert anchors resolve against the surviving set.
    for block_id in &mapping.removed {
        let Some(elem) = find_elem_id(session, *block_id) else {
            continue;
        };
        session.delete_block(elem).map_err(session_err)?;
        ops += 1;
    }

    let matched_by_new: HashMap<usize, BlockId> = mapping
        .matched
        .iter()
        .map(|m| (m.new_index, m.old_id))
        .collect();
    let added_indices: HashSet<usize> = mapping.added.iter().map(|a| a.new_index).collect();

    // Walk file leaf order; insert only newly observed blocks.
    let mut after: Option<OpId> = None;
    for (idx, leaf) in leaves.iter().enumerate() {
        if let Some(old_id) = matched_by_new.get(&idx) {
            if let Some(elem) = find_elem_id(session, *old_id) {
                after = Some(elem);
            }
            continue;
        }
        if !added_indices.contains(&idx) {
            // Should not happen if match covers all new indices.
            continue;
        }
        let elem = insert_leaf_block(session, after, leaf)?;
        // insert_paragraph / insert_block may emit 1–2 ops; count by clock delta is hard.
        // Count logical structure ops: one per added leaf (text may be a second envelope).
        ops += match &leaf.kind {
            BlockKind::Paragraph { text } if !paragraph_visible_string(text).is_empty() => 2,
            _ => 1,
        };
        after = Some(elem);
    }

    Ok(ops)
}

fn insert_leaf_block(
    session: &mut CollaborativeDocument,
    after: Option<OpId>,
    leaf: &Block,
) -> Result<OpId, VaultError> {
    match &leaf.kind {
        BlockKind::Paragraph { text } => {
            let body = paragraph_visible_string(text);
            session.insert_paragraph(after, &body).map_err(session_err)
        }
        BlockKind::CodeFence { info, text } => session
            .insert_block(
                after,
                BlockKind::CodeFence {
                    info: info.clone(),
                    text: text.clone(),
                },
            )
            .map_err(session_err),
        BlockKind::RawBlock { raw } => session
            .insert_block(after, BlockKind::RawBlock { raw: raw.clone() })
            .map_err(session_err),
        BlockKind::Table { .. } => Err(VaultError::UnsupportedIngestBlock("table")),
        BlockKind::BlockQuote { .. } => Err(VaultError::UnsupportedIngestBlock("blockquote")),
    }
}

/// Normalize to a vault-relative path without `..` components.
fn normalize_rel(path: &Path) -> Result<PathBuf, VaultError> {
    if path.is_absolute() {
        return Err(VaultError::InvalidRelativePath(path.to_path_buf()));
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(VaultError::InvalidRelativePath(path.to_path_buf()));
    }
    if path.as_os_str().is_empty() {
        return Err(VaultError::InvalidRelativePath(path.to_path_buf()));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::EquivalenceMode;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn open_creates_stable_peer_id_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "hello").unwrap();

        let s1 = VaultSession::open(dir.path()).unwrap();
        let peer = s1.peer();
        assert_ne!(peer, 0);
        assert!(VaultSession::peer_id_path(&s1.vault).exists());

        let s2 = VaultSession::open(dir.path()).unwrap();
        assert_eq!(s2.peer(), peer);
    }

    #[test]
    fn multi_file_sessions_share_peer_and_are_distinct() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "a").unwrap();
        fs::write(dir.path().join("b.md"), "b").unwrap();

        let mut vs = VaultSession::open(dir.path()).unwrap();
        let peer = vs.peer();

        {
            let a = vs.session_mut("a.md").unwrap();
            assert_eq!(a.peer(), peer);
            a.insert_paragraph(None, "from-a").unwrap();
        }
        {
            let b = vs.session_mut("b.md").unwrap();
            assert_eq!(b.peer(), peer);
            b.insert_paragraph(None, "from-b").unwrap();
        }

        let a_text = vs
            .session_mut("a.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural);
        let b_text = vs
            .session_mut("b.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural);
        assert_eq!(a_text, "from-a");
        assert_eq!(b_text, "from-b");
        assert_eq!(vs.open_paths().count(), 2);
    }

    #[test]
    fn save_and_reopen_restores_document() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "").unwrap();

        {
            let mut vs = VaultSession::open(dir.path()).unwrap();
            vs.session_mut("note.md")
                .unwrap()
                .insert_paragraph(None, "persist me")
                .unwrap();
            vs.save("note.md").unwrap();
        }

        let mut vs = VaultSession::open(dir.path()).unwrap();
        assert!(!vs.is_open("note.md"));
        let peer = vs.peer();
        {
            let doc = vs.session_mut("note.md").unwrap();
            assert_eq!(
                doc.document().serialize(EquivalenceMode::Structural),
                "persist me"
            );
            assert_eq!(doc.peer(), peer);
            assert!(doc.unit_mode());
        }
    }

    #[test]
    fn save_all_persists_every_open_doc() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("x.md"), "").unwrap();
        fs::write(dir.path().join("y.md"), "").unwrap();

        {
            let mut vs = VaultSession::open(dir.path()).unwrap();
            vs.session_mut("x.md")
                .unwrap()
                .insert_paragraph(None, "X")
                .unwrap();
            vs.session_mut("y.md")
                .unwrap()
                .insert_paragraph(None, "Y")
                .unwrap();
            vs.flush_all().unwrap();
        }

        let mut vs = VaultSession::open(dir.path()).unwrap();
        assert_eq!(
            vs.session_mut("x.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Structural),
            "X"
        );
        assert_eq!(
            vs.session_mut("y.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Structural),
            "Y"
        );
    }

    #[test]
    fn rejects_parent_dir_relative_path() {
        let dir = tempdir().unwrap();
        let mut vs = VaultSession::open(dir.path()).unwrap();
        let err = match vs.session_mut("../escape.md") {
            Ok(_) => panic!("expected InvalidRelativePath"),
            Err(e) => e,
        };
        assert!(matches!(err, VaultError::InvalidRelativePath(_)));
    }

    #[test]
    fn save_without_open_errors() {
        let dir = tempdir().unwrap();
        let vs = VaultSession::open(dir.path()).unwrap();
        let err = vs.save("missing.md").unwrap_err();
        assert!(matches!(err, VaultError::SessionNotOpen(_)));
    }
}
