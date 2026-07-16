//! Multi-document vault session: shared peer identity + lazy CollaborativeDocuments.

use super::diff::{delete_indices_high_to_low, graphemes_of, insert_new_indices, lcs_steps};
use super::{
    BlockFingerprint, Fingerprint, IngestReport, LastFlushedState, MatchConfig, ParsedBlock, Score,
    Vault, VaultError, block_content, fingerprint_document, hash_string, match_blocks,
};
use crate::codec::{DocOp, JsonOpCodec, OpBody, OpCodec};
use crate::core::mark::{MarkKind, MarkValue};
use crate::core::{OpId, PeerId, Sequence, StateVector};
use crate::doc::{
    Block, BlockId, BlockKind, Document, ListItem, Parser, RowId, Table, block_id_from_op,
    paragraph_visible_string,
};
use crate::session::{CollaborativeDocument, SessionError, SnapshotError, SyncResponse};
use crate::storage::{Storage, StorageError};
use crate::sync::{ChangeMessage, ValidationLimits};
use crate::workspace::{capture_outline, replace_moved_ids, summarize_outline_change};
use crate::{
    BatchPreview, BatchReceipt, DeletedDocument, DescriptorPage, DiskFingerprint,
    DocumentEditBatch, DocumentExportRequest, DocumentHandle, DocumentId, EditBatch,
    LocalEditOutcome, MultiBatchReceipt, MultiExportOutcome, PreviewToken, ProjectionPage,
    ProjectionRequest, RecoveryReport, RemoteApplyOutcome, RevisionToken, VaultId, WorkspaceEdit,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Shared vault-level identity and open collaborative documents.
///
/// One peer id is stored at `.mdcrdt/peer_id` and used for every file session in
/// this vault. Documents are opened lazily into memory and persisted as
/// [`crate::session::SessionSnapshot`] blobs under `.mdcrdt/sessions/`.
pub struct VaultSession {
    pub vault: Vault,
    vault_id: VaultId,
    /// Stable peer id for this machine/vault (shared by all open docs).
    pub peer: PeerId,
    pub codec: JsonOpCodec,
    /// Lazy map: vault-relative path → session.
    docs: BTreeMap<PathBuf, CollaborativeDocument>,
    document_ids: BTreeMap<PathBuf, DocumentId>,
    revision_cache: BTreeMap<PathBuf, RevisionToken>,
}

impl VaultSession {
    /// Open a vault root, ensure `.mdcrdt` exists, and load or create peer identity.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let vault = Vault::open(path)?;
        vault.init()?;
        recover_pending_transactions(&vault)?;
        let vault_id = load_or_create_identity::<VaultId>(&vault_id_path(&vault))?;
        let peer = load_or_create_peer_id(&vault)?;
        Ok(Self {
            vault,
            vault_id,
            peer,
            codec: JsonOpCodec,
            docs: BTreeMap::new(),
            document_ids: BTreeMap::new(),
            revision_cache: BTreeMap::new(),
        })
    }

    /// Path of the vault-wide peer id file (`.mdcrdt/peer_id`).
    pub fn peer_id_path(vault: &Vault) -> PathBuf {
        vault.path.join(".mdcrdt").join("peer_id")
    }

    pub fn peer(&self) -> PeerId {
        self.peer
    }

    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }

    /// Persistent identity for a vault-relative path, independent of file content.
    pub fn document_id(&mut self, rel_path: impl AsRef<Path>) -> Result<DocumentId, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        if let Some(id) = self.document_ids.get(&rel) {
            return Ok(*id);
        }
        let id = load_or_create_identity::<DocumentId>(&document_id_path(&self.vault, &rel))?;
        self.document_ids.insert(rel, id);
        Ok(id)
    }

    /// Open the concrete workspace document, ingesting disk content for an empty session.
    pub fn open_document(
        &mut self,
        rel_path: impl AsRef<Path>,
    ) -> Result<DocumentHandle, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.document_id(&rel)?;
        let session_empty = self.session(&rel)?.document().blocks_in_order().is_empty();
        let needs_ingest = session_empty
            || self
                .vault
                .read_last_flushed(&self.vault.path.join(&rel))?
                .is_none();
        if needs_ingest {
            self.ingest_file_unchecked(&rel)?;
        }
        self.document_handle(&rel)
    }

    pub fn revision(&mut self, rel_path: impl AsRef<Path>) -> Result<RevisionToken, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.ensure_session_open(&rel)?;
        if let Some(revision) = self.revision_cache.get(&rel) {
            return Ok(revision.clone());
        }
        let revision = revision_for(self.docs.get(&rel).expect("session opened above"))?;
        self.revision_cache.insert(rel, revision.clone());
        Ok(revision)
    }

    /// Return a bounded page of direct, body-free child descriptors.
    pub fn descriptor_page(
        &mut self,
        rel_path: impl AsRef<Path>,
        parent: Option<BlockId>,
        offset: usize,
        limit: usize,
    ) -> Result<DescriptorPage, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.session(&rel)?
            .document()
            .descriptor_page(parent, offset, limit)
            .ok_or_else(|| {
                VaultError::DescriptorParentNotFound(
                    parent.expect("the document root always resolves to a descriptor sequence"),
                )
            })
    }

    /// Create a stable text point from a current grapheme offset.
    pub fn text_point(
        &mut self,
        rel_path: impl AsRef<Path>,
        block_id: BlockId,
        grapheme_offset: usize,
    ) -> Result<crate::TextPoint, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        Ok(self
            .session(&rel)?
            .document()
            .text_point(block_id, grapheme_offset)?)
    }

    /// Create a stable half-open text range from current grapheme offsets.
    pub fn text_range(
        &mut self,
        rel_path: impl AsRef<Path>,
        block_id: BlockId,
        range: std::ops::Range<usize>,
    ) -> Result<crate::TextRange, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        Ok(self.session(&rel)?.document().text_range(block_id, range)?)
    }

    /// Resolve a stable text point against the current document state.
    pub fn resolve_text_point(
        &mut self,
        rel_path: impl AsRef<Path>,
        point: &crate::TextPoint,
    ) -> Result<usize, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        Ok(self.session(&rel)?.document().resolve_text_point(point)?)
    }

    /// Resolve a stable half-open text range against the current document state.
    pub fn resolve_text_range(
        &mut self,
        rel_path: impl AsRef<Path>,
        range: &crate::TextRange,
    ) -> Result<std::ops::Range<usize>, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        Ok(self.session(&rel)?.document().resolve_text_range(range)?)
    }

    /// Capture the complete scoped precondition set required by one edit.
    pub fn preconditions_for_edit(
        &mut self,
        rel_path: impl AsRef<Path>,
        edit: &WorkspaceEdit,
    ) -> Result<Vec<crate::TargetPrecondition>, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        Ok(self
            .session(&rel)?
            .document()
            .preconditions_for_edit(edit)?)
    }

    /// Return one hard-bounded owned semantic projection page for selected block ids.
    pub fn project_blocks(
        &mut self,
        rel_path: impl AsRef<Path>,
        request: ProjectionRequest,
    ) -> Result<ProjectionPage, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let actual_document_id = self.document_id(&rel)?;
        if actual_document_id != request.document_id {
            return Err(VaultError::DocumentIdMismatch {
                expected: request.document_id,
                actual: actual_document_id,
            });
        }
        let actual_revision = self.revision(&rel)?;
        if actual_revision != request.base_revision {
            return Err(VaultError::StaleRevision {
                expected: request.base_revision,
                actual: actual_revision,
            });
        }
        let document = self
            .docs
            .get(&rel)
            .expect("revision verification opens the session")
            .document();
        Ok(document.projection_page(&request, actual_revision)?)
    }

    /// Execute a non-atomic local edit and report only the identities it changed.
    ///
    /// This intentionally carries no revision precondition or rollback guarantee;
    /// preconditioned all-or-nothing edit batches are a separate workspace API.
    pub fn with_local_edit<T>(
        &mut self,
        rel_path: impl AsRef<Path>,
        edit: impl FnOnce(&mut CollaborativeDocument) -> T,
    ) -> Result<LocalEditOutcome<T>, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let (value, changes) = {
            let session = self.session_mut(&rel)?;
            let before = capture_outline(session.document());
            let before_vector = session.state_vector();
            let value = edit(session);
            let changes = summarize_session_transition(session, &before, &before_vector)?;
            (value, changes)
        };
        self.revision_cache
            .insert(rel.clone(), changes.revision.clone());
        self.save_state(&rel)?;
        Ok(LocalEditOutcome { value, changes })
    }

    /// Validate and execute a batch on an isolated session without mutating the vault.
    pub fn preview_edit_batch(
        &mut self,
        rel_path: impl AsRef<Path>,
        batch: &EditBatch,
    ) -> Result<BatchPreview, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let prepared = self.prepare_edit_batch(&rel, batch)?;
        Ok(BatchPreview {
            document_id: batch.document_id,
            revision: prepared.receipt.revision,
            token: prepared.token,
            changes: prepared.receipt.changes,
        })
    }

    /// Atomically apply one preconditioned edit batch to the in-memory document.
    pub fn apply_edit_batch(
        &mut self,
        rel_path: impl AsRef<Path>,
        batch: EditBatch,
    ) -> Result<BatchReceipt, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let prepared = self.prepare_edit_batch(&rel, &batch)?;
        self.install_prepared_batch(rel, prepared)
    }

    /// Apply a batch only when it exactly matches a prior preview token.
    pub fn apply_previewed_batch(
        &mut self,
        rel_path: impl AsRef<Path>,
        batch: EditBatch,
        preview: &PreviewToken,
    ) -> Result<BatchReceipt, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let prepared = self.prepare_edit_batch(&rel, &batch)?;
        if &prepared.token != preview {
            return Err(VaultError::PreviewMismatch);
        }
        self.install_prepared_batch(rel, prepared)
    }

    /// Prevalidate every document batch, then install all prepared sessions together.
    ///
    /// Atomicity is **in-memory**: prevalidation is all-or-nothing (any rejected batch aborts
    /// the whole set with no session swapped and no clock advanced). Snapshot *persistence* is
    /// not crash-atomic — sessions are swapped and then each is `save_state`d in a loop, so a
    /// crash mid-persist can leave some documents flushed and others not. Use
    /// [`Self::export_markdown_transaction`] (journalled) when cross-document durability matters.
    pub fn apply_edit_batches(
        &mut self,
        batches: Vec<DocumentEditBatch>,
    ) -> Result<MultiBatchReceipt, VaultError> {
        let mut seen = HashSet::new();
        let mut prepared = Vec::with_capacity(batches.len());
        for request in &batches {
            let rel = normalize_rel(&request.path)?;
            if !seen.insert(rel.clone()) {
                return Err(VaultError::DuplicateDocumentBatch(rel));
            }
            prepared.push((rel.clone(), self.prepare_edit_batch(&rel, &request.batch)?));
        }
        let mut receipts = Vec::with_capacity(prepared.len());
        for (rel, prepared) in prepared {
            self.revision_cache
                .insert(rel.clone(), prepared.receipt.revision.clone());
            self.docs.insert(rel.clone(), prepared.session);
            receipts.push(prepared.receipt);
        }
        for rel in &seen {
            self.save_state(rel)?;
        }
        Ok(MultiBatchReceipt { receipts })
    }

    fn prepare_edit_batch(
        &mut self,
        rel: &Path,
        batch: &EditBatch,
    ) -> Result<PreparedBatch, VaultError> {
        let actual_document_id = self.document_id(rel)?;
        if actual_document_id != batch.document_id {
            return Err(VaultError::DocumentIdMismatch {
                expected: batch.document_id,
                actual: actual_document_id,
            });
        }
        let actual_revision = self.revision(rel)?;
        let live = self
            .docs
            .get(rel)
            .expect("revision verification opens the session");
        validate_batch_targets(live.document(), batch, &actual_revision)?;
        let previous_revision = actual_revision;
        let before = capture_outline(live.document());
        let before_vector = live.state_vector();
        let snapshot = live
            .save_snapshot()
            .map_err(|error| VaultError::Snapshot(error.to_string()))?;
        let mut probe = CollaborativeDocument::restore_from_snapshot(snapshot)
            .map_err(|error| VaultError::Snapshot(error.to_string()))?;
        for operation in &batch.operations {
            apply_workspace_edit(&mut probe, &operation.edit).map_err(session_err)?;
        }
        let changes = summarize_session_transition(&probe, &before, &before_vector)?;
        let receipt = BatchReceipt {
            document_id: batch.document_id,
            previous_revision,
            revision: changes.revision.clone(),
            changes,
        };
        Ok(PreparedBatch {
            session: probe,
            receipt,
            token: preview_token(batch)?,
        })
    }

    fn install_prepared_batch(
        &mut self,
        rel: PathBuf,
        prepared: PreparedBatch,
    ) -> Result<BatchReceipt, VaultError> {
        write_session_snapshot(&self.vault, &rel, &prepared.session)?;
        self.revision_cache
            .insert(rel.clone(), prepared.receipt.revision.clone());
        self.docs.insert(rel, prepared.session);
        Ok(prepared.receipt)
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
        self.ensure_session_open(&rel)?;
        self.revision_cache.remove(&rel);
        Ok(self.docs.get_mut(&rel).expect("session inserted above"))
    }

    fn session(
        &mut self,
        rel_path: impl AsRef<Path>,
    ) -> Result<&CollaborativeDocument, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.ensure_session_open(&rel)?;
        Ok(self.docs.get(&rel).expect("session inserted above"))
    }

    fn ensure_session_open(&mut self, rel: &Path) -> Result<(), VaultError> {
        self.document_id(rel)?;
        if !self.docs.contains_key(rel) {
            let doc = self.load_or_create_session(rel)?;
            self.docs.insert(rel.to_path_buf(), doc);
        }
        Ok(())
    }

    fn document_handle(&mut self, rel: &Path) -> Result<DocumentHandle, VaultError> {
        let document_id = self.document_id(rel)?;
        let revision = self.revision(rel)?;
        Ok(DocumentHandle {
            vault_id: self.vault_id,
            document_id,
            revision,
            disk_fingerprint: disk_fingerprint(&self.vault.path.join(rel))?,
        })
    }

    /// Refresh one Markdown file after optional workspace and disk precondition checks.
    pub fn refresh_markdown(
        &mut self,
        rel_path: impl AsRef<Path>,
        expected_revision: Option<&RevisionToken>,
        expected_disk_fingerprint: Option<DiskFingerprint>,
    ) -> Result<IngestOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.ingest_markdown(rel, expected_revision, expected_disk_fingerprint)
    }

    /// Ingest one observed Markdown file through revision and disk preconditions.
    pub fn ingest_markdown(
        &mut self,
        rel_path: impl AsRef<Path>,
        expected_revision: Option<&RevisionToken>,
        expected_disk_fingerprint: Option<DiskFingerprint>,
    ) -> Result<IngestOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.verify_revision(&rel, expected_revision)?;
        if let Some(expected) = expected_disk_fingerprint {
            self.verify_disk(&rel, expected)?;
        }
        self.ingest_file_unchecked(&rel)
    }

    /// Durably publish the exact/scoped Markdown view for one document.
    pub fn export_markdown(
        &mut self,
        rel_path: impl AsRef<Path>,
        expected_revision: &RevisionToken,
        expected_disk_fingerprint: Option<DiskFingerprint>,
    ) -> Result<crate::ExportOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.verify_revision(&rel, Some(expected_revision))?;
        if let Some(expected) = expected_disk_fingerprint {
            self.verify_disk(&rel, expected)?;
        }

        let before = capture_outline(
            self.docs
                .get(&rel)
                .expect("revision check opened the session")
                .document(),
        );
        let path = self.vault.path.join(&rel);
        let markdown = self
            .docs
            .get(&rel)
            .expect("revision check opened the session")
            .document()
            .serialize(crate::doc::EquivalenceMode::Exact);
        let prior = fs::read(&path).ok();
        let changed = prior.as_deref() != Some(markdown.as_bytes());
        if changed {
            atomic_write_markdown(&path, markdown.as_bytes(), PublishControl::default())?;
        }

        let parsed = Parser::parse(&markdown);
        self.revision_cache.remove(&rel);
        let session = self.docs.get_mut(&rel).expect("session remains open");
        session.document_mut().adopt_source_from(&parsed);
        let state = LastFlushedState {
            content_hash: hash_string(&markdown),
            blocks: fingerprint_document(session.document()),
        };
        self.vault.write_last_flushed(&path, &state)?;
        self.save_state(&rel)?;

        let document_id = self.document_id(&rel)?;
        let revision = self.revision(&rel)?;
        let after = capture_outline(
            self.docs
                .get(&rel)
                .expect("session remains open after export")
                .document(),
        );
        let changes = summarize_outline_change(&before, &after, 0, revision.clone());
        Ok(crate::ExportOutcome {
            document_id,
            revision,
            disk_fingerprint: disk_fingerprint(&path)?,
            bytes_written: markdown.len(),
            changed,
            changes,
        })
    }

    /// Create a Markdown file durably and initialize its persistent workspace identity.
    pub fn create_markdown(
        &mut self,
        rel_path: impl AsRef<Path>,
        markdown: &str,
    ) -> Result<DocumentHandle, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let path = self.vault.path.join(&rel);
        if path.exists() {
            return Err(VaultError::PathAlreadyExists(path));
        }
        atomic_write_markdown(&path, markdown.as_bytes(), PublishControl::default())?;
        self.open_document(rel)
    }

    /// Rename one document while preserving its persistent `DocumentId` and session state.
    pub fn rename_markdown(
        &mut self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        expected_revision: &RevisionToken,
        expected_disk_fingerprint: Option<DiskFingerprint>,
    ) -> Result<DocumentHandle, VaultError> {
        let from = normalize_rel(from.as_ref())?;
        let to = normalize_rel(to.as_ref())?;
        if self.vault.path.join(&to).exists() {
            return Err(VaultError::PathAlreadyExists(self.vault.path.join(&to)));
        }
        self.verify_revision(&from, Some(expected_revision))?;
        if let Some(expected) = expected_disk_fingerprint {
            self.verify_disk(&from, expected)?;
        }
        let document_id = self.document_id(&from)?;
        self.save_state(&from)?;
        let journal = DurableJournal::Rename {
            from: from.clone(),
            to: to.clone(),
            document_id,
        };
        let journal_path = write_journal(&self.vault, &journal)?;
        if let Err(error) = complete_rename(&self.vault, &from, &to, document_id) {
            return Err(recoverable_error(&journal_path, error));
        }
        if let Err(error) = remove_journal(&journal_path) {
            return Err(recoverable_error(&journal_path, error));
        }

        if let Some(session) = self.docs.remove(&from) {
            self.docs.insert(to.clone(), session);
        }
        if let Some(revision) = self.revision_cache.remove(&from) {
            self.revision_cache.insert(to.clone(), revision);
        }
        self.document_ids.remove(&from);
        self.document_ids.insert(to.clone(), document_id);
        self.document_handle(&to)
    }

    /// Delete one Markdown file and its local workspace artifacts through a recoverable intent.
    pub fn delete_markdown(
        &mut self,
        rel_path: impl AsRef<Path>,
        expected_revision: &RevisionToken,
        expected_disk_fingerprint: Option<DiskFingerprint>,
    ) -> Result<DeletedDocument, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.verify_revision(&rel, Some(expected_revision))?;
        if let Some(expected) = expected_disk_fingerprint {
            self.verify_disk(&rel, expected)?;
        }
        let document_id = self.document_id(&rel)?;
        let journal = DurableJournal::Delete {
            path: rel.clone(),
            document_id,
        };
        let journal_path = write_journal(&self.vault, &journal)?;
        if let Err(error) = complete_delete(&self.vault, &rel) {
            return Err(recoverable_error(&journal_path, error));
        }
        if let Err(error) = remove_journal(&journal_path) {
            return Err(recoverable_error(&journal_path, error));
        }
        self.docs.remove(&rel);
        self.document_ids.remove(&rel);
        self.revision_cache.remove(&rel);
        Ok(DeletedDocument {
            document_id,
            path: rel,
        })
    }

    /// Publish several current document views under one recoverable commit intent.
    pub fn export_markdown_transaction(
        &mut self,
        requests: Vec<DocumentExportRequest>,
    ) -> Result<MultiExportOutcome, VaultError> {
        let mut seen = HashSet::new();
        let mut prepared = Vec::with_capacity(requests.len());
        for request in requests {
            let rel = normalize_rel(&request.path)?;
            if !seen.insert(rel.clone()) {
                return Err(VaultError::DuplicateDocumentBatch(rel));
            }
            let actual_document_id = self.document_id(&rel)?;
            if actual_document_id != request.document_id {
                return Err(VaultError::DocumentIdMismatch {
                    expected: request.document_id,
                    actual: actual_document_id,
                });
            }
            self.verify_revision(&rel, Some(&request.expected_revision))?;
            if let Some(expected) = request.expected_disk_fingerprint {
                self.verify_disk(&rel, expected)?;
            }
            let session = self
                .docs
                .get(&rel)
                .expect("revision verification opens the session");
            let markdown = session
                .document()
                .serialize(crate::doc::EquivalenceMode::Exact);
            let path = self.vault.path.join(&rel);
            let changed = fs::read(&path).ok().as_deref() != Some(markdown.as_bytes());
            prepared.push(PreparedExport {
                rel,
                document_id: request.document_id,
                markdown,
                changed,
                before: capture_outline(session.document()),
            });
        }
        for export in &prepared {
            self.save_state(&export.rel)?;
        }

        // Create the transactions dir before writing any content pending, so its existence
        // reliably signals "a transaction was attempted here" — recovery's orphan sweep then
        // fires even when a crash-before-journal leaves pendings on the very first transaction.
        fs::create_dir_all(transaction_root(&self.vault))?;

        let transaction_id = Uuid::new_v4();
        let mut entries = Vec::new();
        for export in prepared.iter().filter(|export| export.changed) {
            let entry = export_journal_entry(&export.rel, transaction_id)?;
            if let Err(error) = write_synced_file(
                &self.vault.path.join(&entry.pending),
                export.markdown.as_bytes(),
            ) {
                let _ = fs::remove_file(self.vault.path.join(&entry.pending));
                cleanup_pending_entries(&self.vault, &entries);
                return Err(error);
            }
            entries.push(entry);
        }

        let journal_path = if entries.is_empty() {
            None
        } else {
            let journal = DurableJournal::Export {
                entries: entries.clone(),
            };
            match write_journal(&self.vault, &journal) {
                Ok(path) => Some(path),
                Err(error) => {
                    cleanup_pending_entries(&self.vault, &entries);
                    return Err(error);
                }
            }
        };

        if let Some(journal_path) = &journal_path
            && let Err(error) = install_export_entries(&self.vault, &entries)
        {
            return Err(recoverable_error(journal_path, error));
        }

        let outcomes = match self.finalize_exports(prepared) {
            Ok(outcomes) => outcomes,
            Err(error) => {
                return match journal_path {
                    Some(path) => Err(recoverable_error(&path, error)),
                    None => Err(error),
                };
            }
        };
        if let Some(journal_path) = journal_path {
            if let Err(error) = cleanup_export_entries(&self.vault, &entries, &journal_path) {
                return Err(recoverable_error(&journal_path, error));
            }
        }
        Ok(MultiExportOutcome {
            documents: outcomes,
        })
    }

    /// Finish any durable transaction intents left by an interrupted process.
    pub fn recover_transactions(&mut self) -> Result<RecoveryReport, VaultError> {
        recover_pending_transactions(&self.vault)
    }

    fn finalize_exports(
        &mut self,
        prepared: Vec<PreparedExport>,
    ) -> Result<Vec<crate::ExportOutcome>, VaultError> {
        let mut outcomes = Vec::with_capacity(prepared.len());
        for export in prepared {
            let parsed = Parser::parse(&export.markdown);
            self.revision_cache.remove(&export.rel);
            let session = self
                .docs
                .get_mut(&export.rel)
                .expect("prepared export session remains open");
            session.document_mut().adopt_source_from(&parsed);
            let state = LastFlushedState {
                content_hash: hash_string(&export.markdown),
                blocks: fingerprint_document(session.document()),
            };
            let path = self.vault.path.join(&export.rel);
            self.vault.write_last_flushed(&path, &state)?;
            self.save_state(&export.rel)?;
            let revision = self.revision(&export.rel)?;
            let after = capture_outline(
                self.docs
                    .get(&export.rel)
                    .expect("session remains open")
                    .document(),
            );
            let changes = summarize_outline_change(&export.before, &after, 0, revision.clone());
            outcomes.push(crate::ExportOutcome {
                document_id: export.document_id,
                revision,
                disk_fingerprint: disk_fingerprint(&path)?,
                bytes_written: export.markdown.len(),
                changed: export.changed,
                changes,
            });
        }
        Ok(outcomes)
    }

    fn verify_revision(
        &mut self,
        rel: &Path,
        expected: Option<&RevisionToken>,
    ) -> Result<(), VaultError> {
        let Some(expected) = expected else {
            self.session(rel)?;
            return Ok(());
        };
        let actual = self.revision(rel)?;
        if &actual != expected {
            return Err(VaultError::StaleRevision {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(())
    }

    fn verify_disk(&self, rel: &Path, expected: DiskFingerprint) -> Result<(), VaultError> {
        let actual = disk_fingerprint(&self.vault.path.join(rel))?;
        if actual != Some(expected) {
            return Err(VaultError::StaleDisk {
                expected: Some(expected),
                actual,
            });
        }
        Ok(())
    }

    /// State vector for one vault-relative document, opening its session lazily.
    pub fn state_vector(&mut self, rel_path: impl AsRef<Path>) -> Result<StateVector, VaultError> {
        Ok(self.session(rel_path)?.state_vector())
    }

    /// Encode operations for one document that are not covered by `since`.
    pub fn encode_changes_since(
        &mut self,
        rel_path: impl AsRef<Path>,
        since: &StateVector,
    ) -> Result<ChangeMessage, VaultError> {
        Ok(self.session(rel_path)?.encode_changes_since(since)?)
    }

    /// Return an incremental delta or a full checkpoint when the peer is behind history retention.
    pub fn sync_since(
        &mut self,
        rel_path: impl AsRef<Path>,
        since: &StateVector,
    ) -> Result<SyncResponse, VaultError> {
        self.session(rel_path)?
            .sync_since(since)
            .map_err(|error| VaultError::Snapshot(error.to_string()))
    }

    /// Apply remote operations to one document and persist the updated session snapshot.
    pub fn apply_remote(
        &mut self,
        rel_path: impl AsRef<Path>,
        message: ChangeMessage,
        limits: &ValidationLimits,
    ) -> Result<RemoteApplyOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let (result, changes) = {
            let session = self.session_mut(&rel)?;
            let before = capture_outline(session.document());
            let before_vector = session.state_vector();
            let result = session.apply_remote(message, limits).map_err(session_err)?;
            let changes = summarize_session_transition(session, &before, &before_vector)?;
            (result, changes)
        };
        self.revision_cache
            .insert(rel.clone(), changes.revision.clone());
        self.save_state(&rel)?;
        Ok(RemoteApplyOutcome {
            applied: result.applied,
            buffered: result.buffered,
            changes,
        })
    }

    /// Persist one open document's session snapshot to storage.
    pub fn save_state(&self, rel_path: impl AsRef<Path>) -> Result<(), VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let doc = self
            .docs
            .get(&rel)
            .ok_or_else(|| VaultError::SessionNotOpen(rel.clone()))?;
        write_session_snapshot(&self.vault, &rel, doc)
    }

    /// Persist all open document snapshots.
    pub fn save_all_state(&self) -> Result<(), VaultError> {
        for rel in self.docs.keys() {
            let doc = self.docs.get(rel).expect("key from map");
            write_session_snapshot(&self.vault, rel, doc)?;
        }
        Ok(())
    }

    /// Drop an in-memory session without saving (disk snapshot left unchanged).
    pub fn close(&mut self, rel_path: impl AsRef<Path>) -> Result<(), VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        self.docs.remove(&rel);
        self.revision_cache.remove(&rel);
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
            let outcome = self.ingest_file_unchecked(&rel)?;
            if outcome.changed {
                report.files_changed += 1;
                report.ops_emitted += outcome.changes.operation_count;
            } else {
                report.files_noop += 1;
            }
        }
        Ok(report)
    }

    /// Structure ingest for a single vault-relative markdown path.
    ///
    /// Returns `(changed, ops_emitted)`.
    fn ingest_file_unchecked(
        &mut self,
        rel_path: impl AsRef<Path>,
    ) -> Result<IngestOutcome, VaultError> {
        let rel = normalize_rel(rel_path.as_ref())?;
        let abs = self.vault.path.join(&rel);
        if !abs.exists() {
            return Err(VaultError::PathDoesNotExist(abs));
        }
        let content = fs::read_to_string(&abs)?;
        let content_hash = hash_string(&content);
        self.session_mut(&rel)?;
        let before = capture_outline(
            self.docs
                .get(&rel)
                .expect("session opened above")
                .document(),
        );
        let before_vector = self
            .docs
            .get(&rel)
            .expect("session opened above")
            .state_vector();

        if let Some(prev) = self.vault.read_last_flushed(&abs)?
            && prev.content_hash == content_hash
        {
            let needs_source = !self.session_mut(&rel)?.document().has_source_state();
            if needs_source {
                let parsed = Parser::parse(&content);
                self.session_mut(&rel)?
                    .document_mut()
                    .adopt_source_from(&parsed);
                self.save_state(&rel)?;
            }
            let changes = summarize_session_transition(
                self.docs.get(&rel).expect("session remains open"),
                &before,
                &before_vector,
            )?;
            self.revision_cache
                .insert(rel.clone(), changes.revision.clone());
            return Ok(IngestOutcome {
                changed: false,
                changes,
            });
        }

        let parsed = Parser::parse(&content);
        let _ops = {
            let session = self.docs.get_mut(&rel).expect("session opened above");
            let mut ops = sync_frontmatter(session, &parsed)?;
            let empty = session.document().blocks_in_order().is_empty();
            if empty {
                // First ingest: insert the parsed tree recursively (blockquotes preserved).
                let blocks = parsed.blocks_in_order();
                ops += insert_tree(session, None, &blocks)?;
            } else {
                // Re-ingest: recursive structure match (including nested blockquotes).
                ops += apply_structure_ingest(session, &parsed)?;
            }
            ops
        };

        self.docs
            .get_mut(&rel)
            .expect("session still open")
            .document_mut()
            .adopt_source_from(&parsed);

        // Persist session snapshot + fingerprint/hash gate state.
        self.save_state(&rel)?;
        let session = self.docs.get(&rel).expect("session still open");
        let state = LastFlushedState {
            content_hash,
            blocks: fingerprint_document(session.document()),
        };
        self.vault.write_last_flushed(&abs, &state)?;
        let changes = summarize_session_transition(
            self.docs.get(&rel).expect("session remains open"),
            &before,
            &before_vector,
        )?;
        self.revision_cache
            .insert(rel.clone(), changes.revision.clone());
        Ok(IngestOutcome {
            changed: true,
            changes,
        })
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

struct PreparedBatch {
    session: CollaborativeDocument,
    receipt: BatchReceipt,
    token: PreviewToken,
}

struct PreparedExport {
    rel: PathBuf,
    document_id: DocumentId,
    markdown: String,
    changed: bool,
    before: crate::workspace::DocumentOutline,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DurableJournal {
    Export {
        entries: Vec<ExportJournalEntry>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        document_id: DocumentId,
    },
    Delete {
        path: PathBuf,
        document_id: DocumentId,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ExportJournalEntry {
    target: PathBuf,
    pending: PathBuf,
    backup: PathBuf,
}

fn transaction_root(vault: &Vault) -> PathBuf {
    vault.path.join(".mdcrdt").join("transactions")
}

fn write_journal(vault: &Vault, journal: &DurableJournal) -> Result<PathBuf, VaultError> {
    let root = transaction_root(vault);
    fs::create_dir_all(&root)?;
    let id = Uuid::new_v4();
    let path = root.join(format!("{id}.json"));
    let pending = root.join(format!(".{id}.pending"));
    let bytes = serde_json::to_vec(journal).map_err(|_| VaultError::Serialization)?;
    write_synced_file(&pending, &bytes)?;
    fs::rename(&pending, &path)?;
    sync_directory(&root)?;
    Ok(path)
}

fn remove_journal(path: &Path) -> Result<(), VaultError> {
    if path.exists() {
        fs::remove_file(path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn recoverable_error(journal: &Path, error: VaultError) -> VaultError {
    VaultError::RecoverableTransaction {
        journal: journal.to_path_buf(),
        cause: error.to_string(),
    }
}

fn export_journal_entry(
    target: &Path,
    transaction_id: Uuid,
) -> Result<ExportJournalEntry, VaultError> {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| VaultError::InvalidRelativePath(target.to_path_buf()))?;
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    Ok(ExportJournalEntry {
        target: target.to_path_buf(),
        pending: parent.join(format!(".{file_name}.{transaction_id}.pending")),
        backup: parent.join(format!(".{file_name}.{transaction_id}.backup")),
    })
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let parent = path
        .parent()
        .ok_or_else(|| VaultError::InvalidRelativePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_directory(parent)?;
    Ok(())
}

fn install_export_entries(vault: &Vault, entries: &[ExportJournalEntry]) -> Result<(), VaultError> {
    for entry in entries {
        let target = vault.path.join(&entry.target);
        let pending = vault.path.join(&entry.pending);
        let backup = vault.path.join(&entry.backup);
        if pending.exists() {
            if target.exists() && !backup.exists() {
                fs::rename(&target, &backup)?;
            } else if target.exists() {
                fs::remove_file(&target)?;
            }
            fs::rename(&pending, &target)?;
            if let Some(parent) = target.parent() {
                sync_directory(parent)?;
            }
        } else if !target.exists() {
            return Err(VaultError::PathDoesNotExist(target));
        }
    }
    Ok(())
}

fn cleanup_export_entries(
    vault: &Vault,
    entries: &[ExportJournalEntry],
    journal_path: &Path,
) -> Result<(), VaultError> {
    for entry in entries {
        let backup = vault.path.join(&entry.backup);
        if backup.exists() {
            fs::remove_file(&backup)?;
            if let Some(parent) = backup.parent() {
                sync_directory(parent)?;
            }
        }
    }
    remove_journal(journal_path)
}

fn cleanup_pending_entries(vault: &Vault, entries: &[ExportJournalEntry]) {
    for entry in entries {
        let _ = fs::remove_file(vault.path.join(&entry.pending));
    }
}

fn persist_identity(path: &Path, id: DocumentId) -> Result<(), VaultError> {
    if path.exists() {
        let actual = parse_identity::<DocumentId>(path)?;
        return if actual == id {
            Ok(())
        } else {
            Err(VaultError::DocumentIdMismatch {
                expected: id,
                actual,
            })
        };
    }
    let parent = path
        .parent()
        .ok_or_else(|| VaultError::InvalidRelativePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    write_synced_file(path, format!("{id}\n").as_bytes())?;
    sync_directory(parent)?;
    Ok(())
}

fn move_artifact(from: &Path, to: &Path) -> Result<(), VaultError> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        remove_artifact(from)?;
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)?;
    Ok(())
}

fn remove_artifact(path: &Path) -> Result<(), VaultError> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn complete_rename(
    vault: &Vault,
    from: &Path,
    to: &Path,
    document_id: DocumentId,
) -> Result<(), VaultError> {
    let source = vault.path.join(from);
    let destination = vault.path.join(to);
    if source.exists() && destination.exists() {
        return Err(VaultError::PathAlreadyExists(destination));
    }
    persist_identity(&document_id_path(vault, to), document_id)?;
    move_artifact(
        &session_storage_path(vault, from),
        &session_storage_path(vault, to),
    )?;
    move_artifact(
        &vault.state_path_for(&source),
        &vault.state_path_for(&destination),
    )?;
    if source.exists() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&source, &destination)?;
    } else if !destination.exists() {
        return Err(VaultError::PathDoesNotExist(source));
    }
    remove_artifact(&document_id_path(vault, from))?;
    if let Some(parent) = source.parent() {
        sync_directory(parent)?;
    }
    if let Some(parent) = destination.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn complete_delete(vault: &Vault, path: &Path) -> Result<(), VaultError> {
    let markdown = vault.path.join(path);
    remove_artifact(&markdown)?;
    remove_artifact(&document_id_path(vault, path))?;
    remove_artifact(&session_storage_path(vault, path))?;
    remove_artifact(&vault.state_path_for(&markdown))?;
    if let Some(parent) = markdown.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn recover_pending_transactions(vault: &Vault) -> Result<RecoveryReport, VaultError> {
    let root = transaction_root(vault);
    if !root.exists() {
        return Ok(RecoveryReport::default());
    }
    let mut journals: Vec<PathBuf> = fs::read_dir(&root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    journals.sort();
    let mut report = RecoveryReport::default();
    for path in journals {
        let bytes = fs::read(&path)?;
        let journal: DurableJournal =
            serde_json::from_slice(&bytes).map_err(|_| VaultError::Serialization)?;
        match journal {
            DurableJournal::Export { entries } => {
                install_export_entries(vault, &entries)?;
                for entry in &entries {
                    remove_artifact(&vault.state_path_for(&vault.path.join(&entry.target)))?;
                }
                cleanup_export_entries(vault, &entries, &path)?;
                report.files_recovered += entries.len();
            }
            DurableJournal::Rename {
                from,
                to,
                document_id,
            } => {
                complete_rename(vault, &from, &to, document_id)?;
                remove_journal(&path)?;
                report.files_recovered += 1;
            }
            DurableJournal::Delete {
                path: document_path,
                document_id: _,
            } => {
                complete_delete(vault, &document_path)?;
                remove_journal(&path)?;
                report.files_recovered += 1;
            }
        }
        report.transactions_recovered += 1;
    }

    // Crash-before-journal window: export content pendings are fsynced *before* the journal
    // is written, so a crash in between leaves temps no journal will ever reference. Every
    // journal-owned temp was consumed above (install renames pending→target), so any remaining
    // transaction temp is orphan litter. Remove it, matched strictly by the trailing UUID.
    for entry in walkdir::WalkDir::new(&vault.path)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file()
            && let Some(name) = entry.file_name().to_str()
            && is_orphan_transaction_temp(name)
        {
            let _ = fs::remove_file(entry.path());
        }
    }

    Ok(report)
}

/// True for an interrupted-transaction temp file: `.<name>.<uuid>.pending` / `.backup`
/// (export content pending) or `.<uuid>.pending` (journal write temp). Matched strictly by
/// the trailing UUID segment so genuine dotfiles are never removed.
fn is_orphan_transaction_temp(name: &str) -> bool {
    if !name.starts_with('.') {
        return false;
    }
    let stem = name
        .strip_suffix(".pending")
        .or_else(|| name.strip_suffix(".backup"));
    match stem.and_then(|stem| stem.rsplit_once('.')) {
        Some((_, candidate)) => Uuid::parse_str(candidate).is_ok(),
        None => false,
    }
}

fn preview_token(batch: &EditBatch) -> Result<PreviewToken, VaultError> {
    let encoded = serde_json::to_vec(batch).map_err(|_| VaultError::Serialization)?;
    Ok(PreviewToken::from_u128(stable_hash_128(&encoded)))
}

fn validate_batch_targets(
    document: &Document,
    batch: &EditBatch,
    actual_revision: &RevisionToken,
) -> Result<(), VaultError> {
    let revision_matches = &batch.base_revision == actual_revision;
    if batch.operations.is_empty() && !revision_matches {
        return Err(VaultError::StaleRevision {
            expected: batch.base_revision.clone(),
            actual: actual_revision.clone(),
        });
    }
    for (operation_index, operation) in batch.operations.iter().enumerate() {
        if operation.preconditions.is_empty() {
            if revision_matches {
                continue;
            }
            return Err(VaultError::StaleRevision {
                expected: batch.base_revision.clone(),
                actual: actual_revision.clone(),
            });
        }
        let expected = document
            .preconditions_for_edit(&operation.edit)
            .map_err(|source| VaultError::TargetPrecondition {
                operation_index,
                source,
            })?;
        if expected.is_empty() {
            if revision_matches {
                return Err(VaultError::TargetPrecondition {
                    operation_index,
                    source: crate::WorkspaceTargetError::PreconditionMismatch,
                });
            }
            return Err(VaultError::StaleRevision {
                expected: batch.base_revision.clone(),
                actual: actual_revision.clone(),
            });
        }
        if expected != operation.preconditions {
            return Err(VaultError::TargetPrecondition {
                operation_index,
                source: crate::WorkspaceTargetError::PreconditionMismatch,
            });
        }
    }
    Ok(())
}

fn apply_workspace_edit(
    session: &mut CollaborativeDocument,
    operation: &WorkspaceEdit,
) -> Result<(), SessionError> {
    match operation {
        WorkspaceEdit::InsertParagraph {
            parent,
            after,
            text,
        } => {
            let parent = resolve_container(session.document(), *parent)?;
            let after = resolve_block_elem(session.document(), *after)?;
            session.insert_paragraph_in(parent, after, text)?;
        }
        WorkspaceEdit::InsertHeading {
            parent,
            after,
            level,
            text,
        } => {
            if !(1..=6).contains(level) {
                return Err(SessionError::InvalidHeadingLevel);
            }
            let parent = resolve_container(session.document(), *parent)?;
            let after = resolve_block_elem(session.document(), *after)?;
            let elem = session.insert_block_in(
                parent,
                after,
                BlockKind::Heading {
                    level: *level,
                    text: Sequence::new(),
                },
            )?;
            if !text.is_empty() {
                session.insert_text(block_id_from_op(elem), 0, text)?;
            }
        }
        WorkspaceEdit::DeleteBlock { block_id } => {
            let block = session
                .document()
                .find_block_by_id(*block_id)
                .ok_or(SessionError::BlockNotFound)?;
            let parent = session
                .document()
                .block_parent(*block_id)
                .ok_or(SessionError::BlockNotFound)?;
            session.delete_block_in(parent, block.elem_id)?;
        }
        WorkspaceEdit::InsertText { at, text } => {
            let grapheme_offset = session
                .document()
                .resolve_text_point(at)
                .map_err(|_| SessionError::InvalidOffset)?;
            session.insert_text(at.block_id, grapheme_offset, text)?;
        }
        WorkspaceEdit::DeleteText { range } => {
            let resolved = session
                .document()
                .resolve_text_range(range)
                .map_err(|_| SessionError::InvalidOffset)?;
            session.delete_text(
                range.start.block_id,
                resolved.start,
                resolved.end - resolved.start,
            )?;
        }
        WorkspaceEdit::SetMark { range, kind, attrs } => {
            let resolved = session
                .document()
                .resolve_text_range(range)
                .map_err(|_| SessionError::InvalidOffset)?;
            session.set_mark(range.start.block_id, resolved, kind.clone(), attrs.clone())?;
        }
        WorkspaceEdit::RemoveMark {
            block_id,
            interval_id,
        } => {
            session.remove_mark(*block_id, *interval_id)?;
        }
        WorkspaceEdit::SetFrontmatterField { key, value } => {
            session.set_frontmatter_field(key.clone(), value.clone())?;
        }
        WorkspaceEdit::MoveBlock {
            block_id,
            parent,
            after,
        } => {
            let parent = resolve_container(session.document(), *parent)?;
            let after = resolve_block_elem(session.document(), *after)?;
            session.move_block(*block_id, parent, after)?;
        }
        WorkspaceEdit::MoveSection { heading_id, after } => {
            let after = resolve_block_elem(session.document(), *after)?;
            session.move_section(*heading_id, after)?;
        }
        WorkspaceEdit::SplitBlock { at } => {
            let grapheme_offset = session
                .document()
                .resolve_text_point(at)
                .map_err(|_| SessionError::InvalidOffset)?;
            let parent = session
                .document()
                .block_parent(at.block_id)
                .ok_or(SessionError::BlockNotFound)?;
            session.split_block_in(parent, at.block_id, grapheme_offset)?;
        }
        WorkspaceEdit::MergeBlocks { left_id, right_id } => {
            let parent = session
                .document()
                .block_parent(*left_id)
                .ok_or(SessionError::BlockNotFound)?;
            session.merge_blocks_in(parent, *left_id, *right_id)?;
        }
        WorkspaceEdit::InsertTable {
            parent,
            after,
            columns,
            header,
        } => {
            let parent = resolve_container(session.document(), *parent)?;
            let after = resolve_block_elem(session.document(), *after)?;
            session.insert_table_in(parent, after, columns.clone(), header.clone())?;
        }
        WorkspaceEdit::InsertTableRow {
            table_id,
            after,
            cells,
        } => {
            let after = resolve_row_elem(session.document(), *table_id, *after)?;
            session.insert_table_row(*table_id, after, cells.clone())?;
        }
        WorkspaceEdit::SetTableRowCells {
            table_id,
            row_id,
            cells,
        } => {
            let row = resolve_row_elem(session.document(), *table_id, Some(*row_id))?
                .ok_or(SessionError::TableRowNotFound)?;
            session.set_table_row_cells(*table_id, row, cells.clone())?;
        }
        WorkspaceEdit::DeleteTableRow { table_id, row_id } => {
            let row = resolve_row_elem(session.document(), *table_id, Some(*row_id))?
                .ok_or(SessionError::TableRowNotFound)?;
            session.delete_table_row(*table_id, row)?;
        }
        WorkspaceEdit::SetTableMetadata {
            table_id,
            columns,
            header,
        } => {
            session.set_table_metadata(*table_id, columns.clone(), header.clone())?;
        }
        WorkspaceEdit::MoveTableRow {
            table_id,
            row_id,
            after,
        } => {
            let after = resolve_row_elem(session.document(), *table_id, *after)?;
            session.move_table_row(*table_id, *row_id, after)?;
        }
    }
    Ok(())
}

fn resolve_container(
    document: &Document,
    parent: Option<BlockId>,
) -> Result<Option<OpId>, SessionError> {
    parent
        .map(|id| container_elem_by_id(document.blocks(), id).ok_or(SessionError::BlockNotFound))
        .transpose()
}

fn container_elem_by_id(blocks: &Sequence<Block>, id: BlockId) -> Option<OpId> {
    for block in blocks.iter() {
        if block.id == id {
            return matches!(block.kind, BlockKind::BlockQuote { .. }).then_some(block.elem_id);
        }
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                if let Some(found) = container_elem_by_id(children, id) {
                    return Some(found);
                }
            }
            BlockKind::List { items, .. } => {
                for item in items.iter() {
                    if item.id == id {
                        return Some(item.elem_id);
                    }
                    if let Some(found) = container_elem_by_id(&item.children, id) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn resolve_block_elem(
    document: &Document,
    block_id: Option<BlockId>,
) -> Result<Option<OpId>, SessionError> {
    block_id
        .map(|id| {
            document
                .find_block_by_id(id)
                .map(|block| block.elem_id)
                .ok_or(SessionError::BlockNotFound)
        })
        .transpose()
}

fn resolve_row_elem(
    document: &Document,
    table_id: BlockId,
    row_id: Option<RowId>,
) -> Result<Option<OpId>, SessionError> {
    let Some(row_id) = row_id else {
        return Ok(None);
    };
    let block = document
        .find_block_by_id(table_id)
        .ok_or(SessionError::BlockNotFound)?;
    let BlockKind::Table { table } = &block.kind else {
        return Err(SessionError::NotTable);
    };
    table
        .row_by_id(row_id)
        .map(|row| Some(row.elem_id))
        .ok_or(SessionError::TableRowNotFound)
}

trait PersistentIdentity: Copy + std::fmt::Display + std::str::FromStr {
    fn fresh() -> Self;
}

impl PersistentIdentity for VaultId {
    fn fresh() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }
}

impl PersistentIdentity for DocumentId {
    fn fresh() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }
}

fn load_or_create_identity<T>(path: &Path) -> Result<T, VaultError>
where
    T: PersistentIdentity,
{
    if path.exists() {
        return parse_identity(path);
    }
    let id = T::fresh();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("identity");
    let temp = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    writeln!(file, "{id}")?;
    file.sync_all()?;
    drop(file);
    match fs::hard_link(&temp, path) {
        Ok(()) => {
            fs::remove_file(&temp)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temp)?;
            return parse_identity(path);
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(id)
}

fn parse_identity<T>(path: &Path) -> Result<T, VaultError>
where
    T: PersistentIdentity,
{
    let value = fs::read_to_string(path)?;
    value
        .trim()
        .parse()
        .map_err(|_| VaultError::InvalidIdentity {
            path: path.to_path_buf(),
            value: value.trim().to_string(),
        })
}

fn vault_id_path(vault: &Vault) -> PathBuf {
    vault.path.join(".mdcrdt").join("vault_id")
}

fn document_id_path(vault: &Vault, rel: &Path) -> PathBuf {
    let mut path = vault.path.join(".mdcrdt").join("document_ids").join(rel);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map_or_else(|| "id".to_string(), |value| format!("{value}.id"));
    path.set_extension(extension);
    path
}

fn revision_for(document: &CollaborativeDocument) -> Result<RevisionToken, VaultError> {
    let bytes = document
        .save_snapshot()
        .and_then(|snapshot| snapshot.to_bytes())
        .map_err(|error| VaultError::Snapshot(error.to_string()))?;
    Ok(RevisionToken::from_u128(stable_hash_128(&bytes)))
}

fn summarize_session_transition(
    session: &CollaborativeDocument,
    before: &crate::workspace::DocumentOutline,
    before_vector: &StateVector,
) -> Result<crate::ChangeSummary, VaultError> {
    let message = session.encode_changes_since(before_vector)?;
    let operation_count = message.ops.len();
    let mut explicit_moved = std::collections::BTreeSet::new();
    for operation in &message.ops {
        let envelope = JsonOpCodec
            .decode(&operation.payload)
            .map_err(|error| VaultError::Session(error.to_string()))?;
        let OpBody::Doc(doc_op) = envelope.body;
        if let DocOp::MoveBlocks { blocks, .. } = doc_op {
            explicit_moved.extend(blocks.into_iter().map(|block| block.block_id));
        }
    }
    let after = capture_outline(session.document());
    let revision = revision_for(session)?;
    let mut summary = summarize_outline_change(before, &after, operation_count, revision);
    if !explicit_moved.is_empty() {
        replace_moved_ids(&mut summary, before, &after, explicit_moved);
    }
    Ok(summary)
}

fn stable_hash_128(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(*byte)).wrapping_mul(PRIME)
    })
}

fn disk_fingerprint(path: &Path) -> Result<Option<DiskFingerprint>, VaultError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(DiskFingerprint(hash_bytes(&bytes)))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct PublishControl {
    /// Test-only fault injection for the pre-rename crash path. Not present in
    /// production builds so the shipping write path carries no test seam.
    #[cfg(test)]
    fail_before_rename: bool,
}

fn atomic_write_markdown(
    path: &Path,
    bytes: &[u8],
    control: PublishControl,
) -> Result<(), VaultError> {
    let parent = path
        .parent()
        .ok_or_else(|| VaultError::InvalidRelativePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("markdown");
    let temporary = parent.join(format!(".{file_name}.md-crdt.tmp"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    #[cfg(test)]
    if control.fail_before_rename {
        let _ = fs::remove_file(&temporary);
        return Err(VaultError::Io(std::io::Error::other(
            "injected failure before markdown rename",
        )));
    }
    #[cfg(not(test))]
    let _ = control;
    fs::rename(&temporary, path)?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestOutcome {
    pub changed: bool,
    pub changes: crate::ChangeSummary,
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

fn sync_frontmatter(
    session: &mut CollaborativeDocument,
    parsed: &Document,
) -> Result<usize, VaultError> {
    match (&session.document().frontmatter, &parsed.frontmatter) {
        (None, Some(frontmatter)) => {
            session
                .initialize_frontmatter(frontmatter.clone())
                .map_err(session_err)?;
            Ok(1)
        }
        (Some(current), Some(desired)) if current.is_structured() && desired.is_structured() => {
            let current: BTreeMap<String, Option<String>> = current
                .entries()
                .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
                .collect();
            let desired: BTreeMap<String, Option<String>> = desired
                .entries()
                .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
                .collect();
            let keys: HashSet<String> = current.keys().chain(desired.keys()).cloned().collect();
            let mut ops = 0;
            for key in keys {
                let old = current.get(&key).cloned().flatten();
                let new = desired.get(&key).cloned().flatten();
                if old != new {
                    session
                        .set_frontmatter_field(key, new)
                        .map_err(session_err)?;
                    ops += 1;
                }
            }
            Ok(ops)
        }
        (Some(current), None) if current.is_structured() => {
            let keys: Vec<String> = current.entries().map(|(key, _)| key.to_string()).collect();
            let mut ops = 0;
            for key in keys {
                session
                    .set_frontmatter_field(key, None)
                    .map_err(session_err)?;
                ops += 1;
            }
            Ok(ops)
        }
        _ => Ok(0),
    }
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

    // Position-pair residual tables so metadata/cell rewrites keep table and row identities.
    let rem_old_tables: Vec<BlockId> = old
        .iter()
        .filter(|block| {
            removed.contains(&block.id) && matches!(block.kind, BlockKind::Table { .. })
        })
        .map(|block| block.id)
        .collect();
    let rem_new_tables: Vec<usize> = new
        .iter()
        .enumerate()
        .filter(|(index, block)| {
            added.contains(index) && matches!(block.kind, BlockKind::Table { .. })
        })
        .map(|(index, _)| index)
        .collect();
    for (old_id, new_index) in rem_old_tables.into_iter().zip(rem_new_tables) {
        removed.remove(&old_id);
        added.remove(&new_index);
        matched_by_new.insert(new_index, old_id);
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
            let current_elem = session
                .document()
                .find_block_by_id(*old_id)
                .map(|block| block.elem_id)
                .ok_or(VaultError::UnsupportedIngestBlock("missing matched block"))?;
            let current_ids: Vec<OpId> = session
                .document()
                .container_children(parent)
                .ok_or(VaultError::UnsupportedIngestBlock("missing parent"))?
                .iter()
                .map(|block| block.elem_id)
                .collect();
            let predecessor = current_ids
                .iter()
                .position(|id| *id == current_elem)
                .and_then(|position| position.checked_sub(1))
                .map(|position| current_ids[position]);
            let current_elem = if predecessor != after {
                session
                    .move_block(*old_id, parent, after)
                    .map_err(session_err)?;
                ops += 1;
                session
                    .document()
                    .find_block_by_id(*old_id)
                    .expect("moved block remains addressable")
                    .elem_id
            } else {
                current_elem
            };
            after = Some(current_elem);
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
                    ops += sync_tree(session, Some(current_elem), &live_old, &new_refs)?;
                }
                (BlockKind::Table { .. }, BlockKind::Table { table }) => {
                    ops += sync_table(session, ob.id, table)?;
                }
                (BlockKind::Paragraph { text: old_t }, BlockKind::Paragraph { text: new_t })
                | (
                    BlockKind::Heading { text: old_t, .. },
                    BlockKind::Heading { text: new_t, .. },
                ) => {
                    let old_s = paragraph_visible_string(old_t);
                    let new_s = paragraph_visible_string(new_t);
                    ops += apply_paragraph_text_diff(session, ob.id, &old_s, &new_s, nb)?;
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
    desired_block: &Block,
) -> Result<usize, VaultError> {
    let old_g = graphemes_of(old_text);
    let new_g = graphemes_of(new_text);
    let desired_marks = mark_specs(desired_block);
    let current_marks = session
        .document()
        .find_block_by_id(block_id)
        .map(mark_specs)
        .unwrap_or_default();
    if old_g == new_g && mark_semantics(&current_marks) == mark_semantics(&desired_marks) {
        return Ok(0);
    }
    let steps = lcs_steps(&old_g, &new_g);
    let old_to_new: HashMap<usize, usize> = steps
        .iter()
        .filter_map(|step| match step {
            super::diff::GraphemeStep::Equal { old, new } => Some((*old, *new)),
            _ => None,
        })
        .collect();
    let projected = if desired_marks.is_empty() {
        current_marks
            .iter()
            .filter_map(|(_, kind, range, attrs)| {
                let mapped: Vec<usize> = range
                    .clone()
                    .filter_map(|old| old_to_new.get(&old).copied())
                    .collect();
                Some((
                    kind.clone(),
                    *mapped.first()?..mapped.last()?.saturating_add(1),
                    attrs.clone(),
                ))
            })
            .collect::<Vec<_>>()
    } else {
        desired_marks
            .iter()
            .map(|(_, kind, range, attrs)| (kind.clone(), range.clone(), attrs.clone()))
            .collect()
    };
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

    let active_ids: Vec<OpId> = session
        .document()
        .find_block_by_id(block_id)
        .map(|block| {
            block
                .marks
                .iter_active_intervals()
                .map(|mark| mark.id)
                .collect()
        })
        .unwrap_or_default();
    for interval_id in active_ids {
        session
            .remove_mark(block_id, interval_id)
            .map_err(session_err)?;
        ops += 1;
    }
    for (kind, range, attrs) in projected {
        if range.start < range.end && range.end <= new_g.len() {
            session
                .set_mark(block_id, range, kind, attrs)
                .map_err(session_err)?;
            ops += 1;
        }
    }
    Ok(ops)
}

type MarkSpec = (
    OpId,
    MarkKind,
    std::ops::Range<usize>,
    BTreeMap<String, MarkValue>,
);

fn mark_specs(block: &Block) -> Vec<MarkSpec> {
    let Some(text) = crate::doc::block_text_seq(&block.kind) else {
        return Vec::new();
    };
    let ids = crate::doc::paragraph_visible_ids(text);
    block
        .marks
        .resolved_intervals(&ids)
        .into_iter()
        .map(|(interval, start, end)| {
            (
                interval.id,
                interval.kind.clone(),
                start..end,
                interval
                    .attrs
                    .iter()
                    .map(|(key, value)| (key.clone(), value.get()))
                    .collect(),
            )
        })
        .collect()
}

fn mark_semantics(
    specs: &[MarkSpec],
) -> Vec<(
    MarkKind,
    std::ops::Range<usize>,
    BTreeMap<String, MarkValue>,
)> {
    let mut values: Vec<_> = specs
        .iter()
        .map(|(_, kind, range, attrs)| (kind.clone(), range.clone(), attrs.clone()))
        .collect();
    values.sort_by(|left, right| {
        (&left.1.start, &left.1.end, &left.0, &left.2).cmp(&(
            &right.1.start,
            &right.1.end,
            &right.0,
            &right.2,
        ))
    });
    values
}

fn apply_parsed_marks(
    session: &mut CollaborativeDocument,
    parsed: &Block,
    block_id: BlockId,
) -> Result<usize, VaultError> {
    let mut ops = 0;
    for (_, kind, range, attrs) in mark_specs(parsed) {
        session
            .set_mark(block_id, range, kind, attrs)
            .map_err(session_err)?;
        ops += 1;
    }
    Ok(ops)
}

fn sync_table(
    session: &mut CollaborativeDocument,
    table_id: BlockId,
    parsed: &Table,
) -> Result<usize, VaultError> {
    let current = session
        .document()
        .find_block_by_id(table_id)
        .and_then(|block| match &block.kind {
            BlockKind::Table { table } => Some(table.clone()),
            _ => None,
        })
        .ok_or(VaultError::UnsupportedIngestBlock("matched table missing"))?;
    let mut ops = 0usize;
    if current.columns.get_ref() != parsed.columns.get_ref()
        || current.header.get_ref() != parsed.header.get_ref()
    {
        session
            .set_table_metadata(table_id, parsed.columns.get(), parsed.header.get())
            .map_err(session_err)?;
        ops += 1;
    }

    let old_rows = current.rows_in_order();
    let new_rows = parsed.rows_in_order();
    let mut used_old = HashSet::new();
    let mut mapping: Vec<Option<RowId>> = vec![None; new_rows.len()];
    for (new_index, new_row) in new_rows.iter().enumerate() {
        if let Some(old_row) = old_rows.iter().find(|old_row| {
            !used_old.contains(&old_row.id) && old_row.cells.get_ref() == new_row.cells.get_ref()
        }) {
            used_old.insert(old_row.id);
            mapping[new_index] = Some(old_row.id);
        }
    }
    let remaining_old: Vec<_> = old_rows
        .iter()
        .filter(|row| !used_old.contains(&row.id))
        .collect();
    let remaining_new: Vec<_> = mapping
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.is_none().then_some(index))
        .collect();
    for (old_row, new_index) in remaining_old.iter().zip(&remaining_new) {
        used_old.insert(old_row.id);
        mapping[*new_index] = Some(old_row.id);
        if old_row.cells.get_ref() != new_rows[*new_index].cells.get_ref() {
            session
                .set_table_row_cells(table_id, old_row.elem_id, new_rows[*new_index].cells.get())
                .map_err(session_err)?;
            ops += 1;
        }
    }
    for old_row in old_rows.iter().filter(|row| !used_old.contains(&row.id)) {
        session
            .delete_table_row(table_id, old_row.elem_id)
            .map_err(session_err)?;
        ops += 1;
    }
    for new_index in remaining_new.into_iter().skip(remaining_old.len()) {
        let after = mapping[..new_index]
            .iter()
            .rev()
            .flatten()
            .find_map(|row_id| {
                session
                    .document()
                    .find_block_by_id(table_id)
                    .and_then(|block| match &block.kind {
                        BlockKind::Table { table } => {
                            table.row_by_id(*row_id).map(|row| row.elem_id)
                        }
                        _ => None,
                    })
            });
        let elem = session
            .insert_table_row(table_id, after, new_rows[new_index].cells.get())
            .map_err(session_err)?;
        mapping[new_index] = Some(block_id_from_op(elem));
        ops += 1;
    }

    let desired: Vec<RowId> = mapping.into_iter().flatten().collect();
    let mut after = None;
    for row_id in desired {
        let (elem, predecessor) = session
            .document()
            .find_block_by_id(table_id)
            .and_then(|block| match &block.kind {
                BlockKind::Table { table } => {
                    let rows: Vec<_> = table.rows.iter().collect();
                    let position = rows.iter().position(|row| row.id == row_id)?;
                    Some((
                        rows[position].elem_id,
                        position.checked_sub(1).map(|index| rows[index].elem_id),
                    ))
                }
                _ => None,
            })
            .ok_or(VaultError::UnsupportedIngestBlock("row disappeared"))?;
        if predecessor != after {
            session
                .move_table_row(table_id, row_id, after)
                .map_err(session_err)?;
            ops += 1;
            after = session
                .document()
                .find_block_by_id(table_id)
                .and_then(|block| match &block.kind {
                    BlockKind::Table { table } => table.row_by_id(row_id).map(|row| row.elem_id),
                    _ => None,
                });
        } else {
            after = Some(elem);
        }
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
            let marks = apply_parsed_marks(session, block, block_id_from_op(id))?;
            Ok((id, n + marks))
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
            n += apply_parsed_marks(session, block, block_id_from_op(id))?;
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
        BlockKind::Table { table } => {
            let id = session
                .insert_table_in(parent, after, table.columns.get(), table.header.get())
                .map_err(session_err)?;
            let table_id = block_id_from_op(id);
            let mut row_after = None;
            let mut ops = 1;
            for row in table.rows.iter() {
                row_after = Some(
                    session
                        .insert_table_row(table_id, row_after, row.cells.get())
                        .map_err(session_err)?,
                );
                ops += 1;
            }
            Ok((id, ops))
        }
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
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    #[test]
    fn concurrent_identity_creation_returns_one_persisted_value() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity");
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                load_or_create_identity::<VaultId>(&path)
            }));
        }
        let ids: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap())
            .collect();

        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_eq!(parse_identity::<VaultId>(&path).unwrap(), ids[0]);
    }

    #[test]
    fn failed_publication_before_rename_preserves_original_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.md");
        fs::write(&path, "original\n").unwrap();

        let error = atomic_write_markdown(
            &path,
            b"replacement\n",
            PublishControl {
                fail_before_rename: true,
            },
        )
        .unwrap_err();

        assert!(matches!(error, VaultError::Io(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        assert!(!dir.path().join(".note.md.md-crdt.tmp").exists());
    }

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
            vs.save_state("note.md").unwrap();
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
            vs.save_all_state().unwrap();
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
        let err = vs.save_state("missing.md").unwrap_err();
        assert!(matches!(err, VaultError::SessionNotOpen(_)));
    }
}
