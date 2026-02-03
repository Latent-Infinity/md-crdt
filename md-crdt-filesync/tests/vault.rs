use md_crdt_filesync::{Vault, VaultError};
use std::fs;
use std::path::Path;
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
    match result.unwrap_err() {
        VaultError::PathDoesNotExist(path) => {
            assert_eq!(path, non_existent_path);
        }
        other => panic!("Expected PathDoesNotExist error, got {other:?}"),
    }
}
