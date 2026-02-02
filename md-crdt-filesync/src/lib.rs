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
}
