// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration test: verify that git pull on a 50-commit synthetic repo produces < 2000 chars.

use std::path::PathBuf;
use std::process::Command;

/// Create a bare "remote" repo with 50 commits, each touching a unique file.
fn setup_remote(base: &PathBuf) -> PathBuf {
    let remote = base.join("remote.git");
    std::fs::create_dir_all(&remote).expect("create remote dir");

    // init bare repo
    let out = Command::new("git")
        .args(["init", "--bare"])
        .arg(&remote)
        .output()
        .expect("git init --bare");
    assert!(
        out.status.success(),
        "git init --bare failed: {:?}",
        out.stderr
    );

    // init a temp working repo to generate commits
    let work = base.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");

    let run = |args: &[&str], dir: &PathBuf| {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("command {:?} failed: {}", args, e));
        assert!(
            out.status.success(),
            "command {:?} failed in {:?}: {}",
            args,
            dir,
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run(&["git", "init"], &work);
    run(&["git", "config", "user.email", "hugues@linux.com"], &work);
    run(&["git", "config", "user.name", "Test"], &work);
    // Disable GPG signing and hooks for the synthetic test repo
    run(&["git", "config", "commit.gpgsign", "false"], &work);
    run(&["git", "config", "core.hooksPath", "/dev/null"], &work);

    let remote_str = remote.to_string_lossy().into_owned();
    run(&["git", "remote", "add", "origin", &remote_str], &work);

    // Generate 50 commits (use conventional commit format for local hooks)
    for i in 0..50u32 {
        let filename = format!("file{:03}.txt", i);
        std::fs::write(work.join(&filename), format!("content {}", i)).expect("write file");
        run(&["git", "add", &filename], &work);
        run(
            &["git", "commit", "-m", &format!("chore: add file {:03}", i)],
            &work,
        );
    }

    // Push to bare remote
    run(&["git", "push", "origin", "HEAD:main"], &work);

    remote
}

/// Create a local clone of the remote with 1 commit, so pull has 49 commits to fetch.
fn setup_local(base: &PathBuf, remote: &PathBuf) -> PathBuf {
    let local = base.join("local");

    let remote_str = remote.to_string_lossy().into_owned();
    let out = Command::new("git")
        .args(["clone", &remote_str, "local"])
        .current_dir(base)
        .output()
        .expect("git clone");
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Configure local identity
    let run_local = |args: &[&str]| {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(&local)
            .output()
            .unwrap_or_else(|e| panic!("command {:?} failed: {}", args, e));
        assert!(
            out.status.success(),
            "command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run_local(&["git", "config", "user.email", "hugues@linux.com"]);
    run_local(&["git", "config", "user.name", "Test"]);

    // Reset local to first commit so there is something to pull
    let out = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&local)
        .output()
        .expect("git log");
    let log = String::from_utf8_lossy(&out.stdout);
    let commits: Vec<&str> = log.lines().collect();
    // Move local HEAD back by 20 commits (reset to 30th commit)
    let target = commits
        .get(19)
        .map(|l| l.split_whitespace().next().unwrap_or("HEAD"));
    if let Some(sha) = target {
        let _ = Command::new("git")
            .args(["reset", "--hard", sha])
            .current_dir(&local)
            .output();
    }

    local
}

#[test]
fn test_exec_git_pull_volume_guard() {
    // Arrange: synthetic 50-commit git repo with local clone behind by ~20 commits
    let base = std::env::temp_dir().join(format!(
        "aptu-exec-volume-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create base dir");

    let remote = setup_remote(&base);
    let local = setup_local(&base, &remote);

    // Act: run git pull in the local repo via std::process::Command
    let out = Command::new("git")
        .args(["pull", "--no-stat", "origin", "main"])
        .current_dir(&local)
        .output()
        .expect("git pull");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined_len = stdout.len() + stderr.len();

    // Assert: combined output length < 2000 chars (validates volume; NOT asserting output_truncated==false)
    assert!(
        combined_len < 2000,
        "git pull output should be < 2000 chars, got {} (stdout={} + stderr={})\nstdout: {}\nstderr: {}",
        combined_len,
        stdout.len(),
        stderr.len(),
        &stdout,
        &stderr,
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&base);
}
