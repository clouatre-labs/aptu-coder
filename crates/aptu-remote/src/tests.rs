// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use super::{Platform, RemoteError, detect_platform, parse_line_range, slice_lines};

// ---------------------------------------------------------------------------
// Platform detection tests
// ---------------------------------------------------------------------------

#[test]
fn test_detect_platform_gitlab() {
    let (platform, owner, repo) =
        detect_platform("https://gitlab.com/org/repo").expect("should parse");
    assert!(
        matches!(platform, Platform::GitLab { host } if host == "gitlab.com"),
        "expected GitLab platform"
    );
    assert_eq!(owner, "org");
    assert_eq!(repo, "repo");
}

#[test]
fn test_detect_platform_github() {
    let (platform, owner, repo) =
        detect_platform("https://github.com/org/repo").expect("should parse");
    assert!(
        matches!(platform, Platform::GitHub),
        "expected GitHub platform"
    );
    assert_eq!(owner, "org");
    assert_eq!(repo, "repo");
}

#[test]
fn test_detect_platform_git_suffix() {
    let (_, owner, repo) =
        detect_platform("https://github.com/org/my-repo.git").expect("should parse");
    assert_eq!(owner, "org");
    assert_eq!(repo, "my-repo");
}

#[test]
fn test_detect_platform_invalid() {
    let err = detect_platform("https://example.com/o/r").expect_err("should fail");
    assert!(
        matches!(err, RemoteError::UnsupportedHost(_)),
        "expected UnsupportedHost, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Line range slicing tests
// ---------------------------------------------------------------------------

#[test]
fn test_line_range_slicing() {
    let content = "line1\nline2\nline3\nline4";
    let result = slice_lines(content, 2, 3);
    assert_eq!(result, "line2\nline3");
}

#[test]
fn test_line_range_out_of_bounds() {
    let content = "line1\nline2\nline3";
    // Requesting beyond the last line should return whatever is available
    let result = slice_lines(content, 2, 100);
    assert_eq!(result, "line2\nline3");
}

// ---------------------------------------------------------------------------
// parse_line_range tests
// ---------------------------------------------------------------------------

#[test]
fn test_parse_line_range_valid() {
    let (start, end) = parse_line_range("10-50").expect("should parse");
    assert_eq!(start, 10);
    assert_eq!(end, 50);
}

#[test]
fn test_parse_line_range_invalid_non_numeric() {
    assert!(parse_line_range("abc-def").is_err());
}

#[test]
fn test_parse_line_range_invalid_no_dash() {
    assert!(parse_line_range("50").is_err());
}

#[test]
fn test_parse_line_range_invalid_end_less_than_start() {
    let err = parse_line_range("50-10").expect_err("should fail");
    assert!(
        matches!(err, RemoteError::InvalidLineRange(_)),
        "expected InvalidLineRange, got: {err}"
    );
}

#[test]
fn test_parse_line_range_invalid_zero_start() {
    let err = parse_line_range("0-10").expect_err("should fail");
    assert!(
        matches!(err, RemoteError::InvalidLineRange(_)),
        "expected InvalidLineRange, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Missing token tests
// ---------------------------------------------------------------------------

#[test]
fn test_missing_gitlab_token() {
    // Remove the token from the environment for this test
    let key = "GITLAB_TOKEN";
    let saved = std::env::var(key).ok();
    // SAFETY: single-threaded test; std::env is not thread-safe but isolation
    // is acceptable here given the test binary is not run with --test-threads > 1
    // for this crate.
    unsafe {
        std::env::remove_var(key);
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(crate::fetch_tree("https://gitlab.com/o/r", None, None, 2));

    // Restore the env var
    if let Some(v) = saved {
        unsafe {
            std::env::set_var(key, v);
        }
    }

    let err = result.expect_err("should fail with missing token");
    assert!(
        matches!(err, RemoteError::MissingGitLabToken),
        "expected MissingGitLabToken, got: {err}"
    );
}

#[test]
fn test_missing_github_token() {
    let key = "GITHUB_TOKEN";
    let saved = std::env::var(key).ok();
    unsafe {
        std::env::remove_var(key);
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(crate::fetch_tree("https://github.com/o/r", None, None, 2));

    if let Some(v) = saved {
        unsafe {
            std::env::set_var(key, v);
        }
    }

    let err = result.expect_err("should fail with missing token");
    assert!(
        matches!(err, RemoteError::MissingGitHubToken),
        "expected MissingGitHubToken, got: {err}"
    );
}
