//! Multi-document vault session: shared peer identity + lazy CollaborativeDocuments.

use super::diff::{delete_indices_high_to_low, graphemes_of, insert_new_indices, lcs_steps};
use super::{
    BlockFingerprint, Fingerprint, IngestReport, LastFlushedState, MatchConfig, ParsedBlock, Score,
    Vault, VaultError, block_content, fingerprint_document, hash_string, match_blocks,
};
use crate::codec::JsonOpCodec;
use crate::core::{OpId, PeerId, Sequence};
use crate::doc::{Block, BlockId, BlockKind, Document, ListItem, Parser, paragraph_visible_string};
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
        let ops = {
            let session = self.docs.get_mut(&rel).expect("session opened above");
            let empty = session.document().blocks_in_order().is_empty();
            if empty {
                // First ingest: insert the parsed tree recursively (blockquotes preserved).
                let blocks = parsed.blocks_in_order();
                insert_tree(session, None, &blocks)?
            } else {
                // Re-ingest: recursive structure match (including nested blockquotes).
                apply_structure_ingest(session, &parsed)?
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
    /// File contains not-yet-ingestable blocks (e.g. tables).
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

/// Insert a parsed block tree into `parent`'s children (top-level when `None`),
/// preserving blockquote nesting. Returns an approximate structure-op count.
/// Insert a sequence of parsed blocks into `parent`'s children (top-level when `None`),
/// preserving blockquote nesting. Returns an approximate structure-op count.
fn insert_tree(
    session: &mut CollaborativeDocument,
    parent: Option<OpId>,
    blocks: &[&Block],
) -> Result<usize, VaultError> {
    let mut ops = 0usize;
    let mut after: Option<OpId> = None;
    for block in blocks {
        let (elem, n) = insert_one(session, parent, after, block)?;
        ops += n;
        after = Some(elem);
    }
    Ok(ops)
}

/// Structure-only re-ingest of a full document tree (including nested blockquotes).
///
/// Matched leaves keep CRDT identity; text edits on matched leaves are deferred to LCS.
/// Blockquotes match as containers (content-agnostic fingerprint) so children can be
/// reconciled without replacing the quote.
fn apply_structure_ingest(
    session: &mut CollaborativeDocument,
    parsed: &Document,
) -> Result<usize, VaultError> {
    let old: Vec<Block> = session
        .document()
        .blocks_in_order()
        .into_iter()
        .cloned()
        .collect();
    let new = parsed.blocks_in_order();
    sync_tree(session, None, &old, &new)
}

/// Fingerprint used at one tree level. Quotes are structure tokens so nested text
/// edits do not destroy the container match.
fn level_match_content(kind: &BlockKind) -> String {
    match kind {
        BlockKind::BlockQuote { .. } => "blockquote".to_string(),
        other => block_content(other),
    }
}

fn level_old_state(blocks: &[Block]) -> LastFlushedState {
    LastFlushedState {
        content_hash: 0,
        blocks: blocks
            .iter()
            .enumerate()
            .map(|(i, b)| BlockFingerprint {
                block_id: b.id,
                fingerprint: Fingerprint::from_content(&level_match_content(&b.kind)),
                container_path: Vec::new(),
                position: i,
            })
            .collect(),
    }
}

fn level_new_parsed(blocks: &[&Block]) -> Vec<ParsedBlock> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, b)| ParsedBlock {
            fingerprint: Fingerprint::from_content(&level_match_content(&b.kind)),
            container_path: Vec::new(),
            position: i,
        })
        .collect()
}

fn sync_tree(
    session: &mut CollaborativeDocument,
    parent: Option<OpId>,
    old: &[Block],
    new: &[&Block],
) -> Result<usize, VaultError> {
    // Content floor: position alone cannot pair zero-similarity leaves.
    let config = MatchConfig {
        min_match_score: Score(5000),
        ..MatchConfig::default()
    };
    let mapping = match_blocks(&level_old_state(old), &level_new_parsed(new), &config);

    let old_by_id: HashMap<BlockId, &Block> = old.iter().map(|b| (b.id, b)).collect();
    let mut matched_by_new: HashMap<usize, BlockId> = mapping
        .matched
        .iter()
        .map(|m| (m.new_index, m.old_id))
        .collect();
    let mut removed: HashSet<BlockId> = mapping.removed.iter().copied().collect();
    let mut added: HashSet<usize> = mapping.added.iter().map(|a| a.new_index).collect();

    // Position-pair remaining paragraphs so in-place text edits keep BlockId and use LCS.
    let rem_old_para: Vec<BlockId> = old
        .iter()
        .filter(|b| removed.contains(&b.id) && matches!(b.kind, BlockKind::Paragraph { .. }))
        .map(|b| b.id)
        .collect();
    let rem_new_para: Vec<usize> = new
        .iter()
        .enumerate()
        .filter(|(i, b)| added.contains(i) && matches!(b.kind, BlockKind::Paragraph { .. }))
        .map(|(i, _)| i)
        .collect();
    let pair_n = rem_old_para.len().min(rem_new_para.len());
    for k in 0..pair_n {
        let old_id = rem_old_para[k];
        let new_idx = rem_new_para[k];
        removed.remove(&old_id);
        added.remove(&new_idx);
        matched_by_new.insert(new_idx, old_id);
    }

    let mut ops = 0usize;

    // Structural deletes (unpaired removed blocks) first.
    for block_id in &removed {
        let Some(b) = old_by_id.get(block_id) else {
            continue;
        };
        session
            .delete_block_in(parent, b.elem_id)
            .map_err(session_err)?;
        ops += 1;
    }

    let mut after: Option<OpId> = None;
    for (idx, nb) in new.iter().enumerate() {
        if let Some(old_id) = matched_by_new.get(&idx) {
            let Some(ob) = old_by_id.get(old_id) else {
                continue;
            };
            after = Some(ob.elem_id);
            match (&ob.kind, &nb.kind) {
                (
                    BlockKind::BlockQuote { children: old_kids },
                    BlockKind::BlockQuote { children: new_kids },
                ) => {
                    let live_old: Vec<Block> = session
                        .document()
                        .find_block(ob.elem_id)
                        .and_then(|b| match &b.kind {
                            BlockKind::BlockQuote { children } => {
                                Some(children.iter_asc().cloned().collect())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| old_kids.iter_asc().cloned().collect());
                    let new_refs: Vec<&Block> = new_kids.iter_asc().collect();
                    ops += sync_tree(session, Some(ob.elem_id), &live_old, &new_refs)?;
                }
                (BlockKind::Paragraph { text: old_t }, BlockKind::Paragraph { text: new_t }) => {
                    let old_s = paragraph_visible_string(old_t);
                    let new_s = paragraph_visible_string(new_t);
                    if old_s != new_s {
                        ops += apply_paragraph_text_diff(session, ob.id, &old_s, &new_s)?;
                    }
                }
                // Matched non-paragraph leaves with different content: leave as-is for now
                // (code/raw full replace would be delete+insert; content match already paired equals).
                _ => {}
            }
            continue;
        }
        if !added.contains(&idx) {
            continue;
        }
        let (elem, n) = insert_one(session, parent, after, nb)?;
        ops += n;
        after = Some(elem);
    }

    Ok(ops)
}

/// Apply grapheme LCS between current paragraph body and `new_text` (InsertText/DeleteText).
///
/// Preserves OpIds for LCS-equal units. Marks on deleted units may be dropped (documented).
fn apply_paragraph_text_diff(
    session: &mut CollaborativeDocument,
    block_id: BlockId,
    old_text: &str,
    new_text: &str,
) -> Result<usize, VaultError> {
    let old_g = graphemes_of(old_text);
    let new_g = graphemes_of(new_text);
    if old_g == new_g {
        return Ok(0);
    }
    let steps = lcs_steps(&old_g, &new_g);
    let mut ops = 0usize;

    // Deletes first (high index → low) so offsets stay valid.
    for i in delete_indices_high_to_low(&steps) {
        session.delete_text(block_id, i, 1).map_err(session_err)?;
        ops += 1;
    }

    // After deletes, live body is the LCS sequence. Insert missing new graphemes left→right.
    let mut live_pos = 0usize;
    let insert_set: HashSet<usize> = insert_new_indices(&steps).into_iter().collect();
    let mut run = String::new();
    let mut run_at: Option<usize> = None;
    for (j, g) in new_g.iter().enumerate() {
        if insert_set.contains(&j) {
            if run_at.is_none() {
                run_at = Some(live_pos);
            }
            run.push_str(g);
        } else {
            if let Some(at) = run_at.take() {
                session
                    .insert_text(block_id, at, &run)
                    .map_err(session_err)?;
                live_pos = at + graphemes_of(&run).len();
                run.clear();
                ops += 1;
            }
            live_pos += 1;
        }
    }
    if let Some(at) = run_at {
        session
            .insert_text(block_id, at, &run)
            .map_err(session_err)?;
        ops += 1;
    }
    Ok(ops)
}

/// Insert a single parsed block (and nested children for quotes). Returns `(elem_id, op_count)`.
fn insert_one(
    session: &mut CollaborativeDocument,
    parent: Option<OpId>,
    after: Option<OpId>,
    block: &Block,
) -> Result<(OpId, usize), VaultError> {
    match &block.kind {
        BlockKind::Paragraph { text } => {
            let body = paragraph_visible_string(text);
            let n = if body.is_empty() { 1 } else { 2 };
            let id = session
                .insert_paragraph_in(parent, after, &body)
                .map_err(session_err)?;
            Ok((id, n))
        }
        BlockKind::Heading { level, text } => {
            // Insert heading skeleton with body via insert_block + insert_text when non-empty.
            let body = paragraph_visible_string(text);
            let id = session
                .insert_block_in(
                    parent,
                    after,
                    BlockKind::Heading {
                        level: *level,
                        text: Sequence::new(),
                    },
                )
                .map_err(session_err)?;
            let mut n = 1;
            if !body.is_empty() {
                let bid = crate::doc::block_id_from_op(id);
                session.insert_text(bid, 0, &body).map_err(session_err)?;
                n += 1;
            }
            Ok((id, n))
        }
        BlockKind::List { ordered, items } => {
            // Insert the list with session-allocated, contiguous item ids and empty item
            // children, then insert each item's children (paragraph text via InsertText).
            // Avoids unit-mode text stripping and keeps the list body syncable.
            let peer = session.peer();
            let base = session.peek_next_id().counter; // == the list block's own counter
            let ordered_items: Vec<&ListItem> = items.iter().collect();
            let mut item_elems: Vec<OpId> = Vec::new();
            let mut empty: Vec<(OpId, ListItem)> = Vec::new();
            for i in 0..ordered_items.len() {
                let elem = OpId {
                    counter: base + 1 + i as u64,
                    peer,
                };
                item_elems.push(elem);
                empty.push((
                    elem,
                    ListItem {
                        id: crate::doc::block_id_from_op(elem),
                        elem_id: elem,
                        children: Sequence::new(),
                    },
                ));
            }
            let list_elem = session
                .insert_block_in(
                    parent,
                    after,
                    BlockKind::List {
                        ordered: *ordered,
                        items: Sequence::from_ordered(empty),
                    },
                )
                .map_err(session_err)?;
            let mut n = 1;
            for (it, item_elem) in ordered_items.iter().zip(item_elems.iter()) {
                let kids: Vec<&Block> = it.children.iter_asc().collect();
                n += insert_tree(session, Some(*item_elem), &kids)?;
            }
            Ok((list_elem, n))
        }
        BlockKind::CodeFence { info, text } => {
            let id = session
                .insert_block_in(
                    parent,
                    after,
                    BlockKind::CodeFence {
                        info: info.clone(),
                        text: text.clone(),
                    },
                )
                .map_err(session_err)?;
            Ok((id, 1))
        }
        BlockKind::RawBlock { raw } => {
            let id = session
                .insert_block_in(parent, after, BlockKind::RawBlock { raw: raw.clone() })
                .map_err(session_err)?;
            Ok((id, 1))
        }
        BlockKind::BlockQuote { children } => {
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
            let nested = insert_tree(session, Some(q), &kids)?;
            Ok((q, 1 + nested))
        }
        BlockKind::Table { .. } => Err(VaultError::UnsupportedIngestBlock("table")),
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
