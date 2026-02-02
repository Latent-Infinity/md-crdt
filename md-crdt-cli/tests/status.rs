use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
#[allow(deprecated)]
fn test_status_untracked_files() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "content1").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("status").current_dir(dir.path());

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Untracked: file1.md"));
}

#[test]
#[allow(deprecated)]
fn test_status_json_output() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "content1").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("status").arg("--json").current_dir(dir.path());

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1));

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.is_object());
    let files = json.get("files").unwrap();
    assert!(files.is_array());
    let file1 = files.get(0).unwrap();
    assert_eq!(file1.get("path").unwrap(), "file1.md");
    assert_eq!(file1.get("status").unwrap(), "Untracked");
}

#[test]
#[allow(deprecated)]
fn test_status_clean_vault() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt").join("state")).unwrap();
    fs::write(dir.path().join("file1.md"), "content1").unwrap();
    fs::write(
        dir.path()
            .join(".mdcrdt")
            .join("state")
            .join("file1.mdcrdt"),
        "state",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("status").current_dir(dir.path());

    cmd.assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("Vault is clean."));
}

#[test]
#[allow(deprecated)]
fn test_status_json_clean_vault() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt").join("state")).unwrap();
    fs::write(dir.path().join("file1.md"), "content1").unwrap();
    fs::write(
        dir.path()
            .join(".mdcrdt")
            .join("state")
            .join("file1.mdcrdt"),
        "state",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("status").arg("--json").current_dir(dir.path());

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(0));

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.is_object());
    let files = json.get("files").unwrap();
    assert!(files.is_array());
    let file1 = files.get(0).unwrap();
    assert_eq!(file1.get("status").unwrap(), "Tracked");
}

#[test]
#[allow(deprecated)]
fn test_status_multiple_files_mixed() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt").join("state")).unwrap();
    fs::write(dir.path().join("tracked.md"), "content1").unwrap();
    fs::write(dir.path().join("untracked.md"), "content2").unwrap();
    fs::write(
        dir.path()
            .join(".mdcrdt")
            .join("state")
            .join("tracked.mdcrdt"),
        "state",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("status").current_dir(dir.path());

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Untracked: untracked.md"));
}
