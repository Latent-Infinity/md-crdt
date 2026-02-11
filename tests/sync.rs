use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
#[allow(deprecated)]
fn test_init_creates_state_dir() {
    let dir = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("init").current_dir(dir.path());

    cmd.assert().success().code(0);
    assert!(dir.path().join(".mdcrdt").join("state").exists());
}

#[test]
#[allow(deprecated)]
fn test_flush_and_ingest_exit_codes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().success().code(0);

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert().success().code(0);

    fs::write(dir.path().join("file1.md"), "changed").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert().success().code(0);
}

#[test]
#[allow(deprecated)]
fn test_sync_exit_codes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().success().code(0);

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("sync").current_dir(dir.path());
    cmd.assert().success().code(0);

    fs::write(dir.path().join("file1.md"), "changed").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("sync").current_dir(dir.path());
    cmd.assert().failure().code(2);
}

#[test]
#[allow(deprecated)]
fn test_command_errors() {
    let dir = tempdir().unwrap();
    let bad_file = dir.path().join("file1.md");
    fs::write(&bad_file, [0xFF]).unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().failure().code(1);

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert().failure().code(1);

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("sync").current_dir(dir.path());
    cmd.assert().failure().code(1);
}

#[test]
#[allow(deprecated)]
fn test_flush_ingest_output_messages() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().stdout(predicate::str::contains("Flushed"));

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert().stdout(predicate::str::contains("Ingest"));
}

#[test]
#[allow(deprecated)]
fn test_init_command_success() {
    let dir = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("init").current_dir(dir.path());
    cmd.assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("Initialized"));
}

#[test]
#[allow(deprecated)]
fn test_ingest_no_changes_message() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    // First flush to record state
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().success();

    // Ingest should show no changes
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("no changes"));
}

#[test]
#[allow(deprecated)]
fn test_ingest_changes_detected_message() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    // First flush to record state
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().success();

    // Modify the file
    fs::write(dir.path().join("file1.md"), "modified").unwrap();

    // Ingest should detect changes
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("ingest").current_dir(dir.path());
    cmd.assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("changes detected"));
}

#[test]
#[allow(deprecated)]
fn test_sync_clean_message() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file1.md"), "hello").unwrap();

    // First flush to record state
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("flush").current_dir(dir.path());
    cmd.assert().success();

    // Sync should show clean
    let mut cmd = Command::cargo_bin("md-crdt").unwrap();
    cmd.arg("sync").current_dir(dir.path());
    cmd.assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("clean"));
}
