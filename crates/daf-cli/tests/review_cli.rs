use assert_cmd::Command;
use predicates::prelude::*;
#[test]
fn review_help_exposes_resume_controls() {
    Command::cargo_bin("daf")
        .unwrap()
        .args(["review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--state-dir"))
        .stdout(predicate::str::contains("--allow-unknown-retry"))
        .stdout(predicate::str::contains("--provider-executable"));
}
#[test]
fn review_requires_state_directory() {
    Command::cargo_bin("daf")
        .unwrap()
        .arg("review")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--state-dir"));
}
#[test]
fn review_invalid_repository_fails_before_provider() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("daf")
        .unwrap()
        .args(["review", "--repo"])
        .arg(temp.path().join("missing"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .arg("--provider-executable")
        .arg("must-never-be-launched")
        .assert()
        .failure();
}
