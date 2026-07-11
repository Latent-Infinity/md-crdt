//! Multi-document vault session: shared peer identity + lazy CollaborativeDocuments.

use super::{Vault, VaultError};
use crate::codec::JsonOpCodec;
use crate::core::PeerId;
use crate::session::{CollaborativeDocument, SnapshotError};
use crate::storage::{Storage, StorageError};
use std::collections::BTreeMap;
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
