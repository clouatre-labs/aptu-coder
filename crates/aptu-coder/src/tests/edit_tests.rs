use crate::validation::{validate_path, validate_path_relative_to};

#[test]
fn test_validate_path_rejects_absolute_path_outside_cwd() {
    // S4: Verify that absolute paths outside the current working directory are rejected.
    // This test directly calls validate_path with /etc/passwd, which should fail.
    let result = validate_path("/etc/passwd", true);
    assert!(
        result.is_err(),
        "validate_path should reject /etc/passwd (outside CWD)"
    );
    let err = result.unwrap_err();
    let err_msg = err.message.to_lowercase();
    assert!(
        err_msg.contains("outside") || err_msg.contains("not found"),
        "Error message should mention 'outside' or 'not found': {}",
        err.message
    );
}

#[test]
fn test_validate_path_accepts_relative_path_in_cwd() {
    // Happy path: relative path within CWD should be accepted.
    // Use Cargo.toml which exists in the crate root.
    let result = validate_path("Cargo.toml", true);
    assert!(
        result.is_ok(),
        "validate_path should accept Cargo.toml (exists in CWD)"
    );
}

#[test]
fn test_validate_path_creates_parent_for_nonexistent_file() {
    // Edge case: non-existent file with existing parent should be accepted.
    let cwd = std::env::current_dir().expect("should get cwd");
    let parent = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let parent_path = parent.path().to_path_buf();
    let child = parent_path.join("new_file.txt");

    let child_str = child.to_str().expect("path should be valid UTF-8");
    let result = validate_path(child_str, false);
    assert!(
        result.is_ok(),
        "validate_path should accept non-existent file with existing parent (require_exists=false)"
    );
    let path = result.unwrap();
    let canonical_cwd = std::fs::canonicalize(&cwd).expect("should canonicalize cwd");
    assert!(
        path.starts_with(&canonical_cwd),
        "Resolved path should be within CWD: {:?} should start with {:?}",
        path,
        canonical_cwd
    );
}

#[test]
fn test_edit_overwrite_with_working_dir() {
    // Arrange: create a temporary directory within CWD to use as working_dir
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_path = temp_dir.path();

    // Act: call validate_path_relative_to with a relative path
    let result = validate_path_relative_to("test_file.txt", false, temp_path);

    // Assert: path should be resolved relative to working_dir
    assert!(
        result.is_ok(),
        "validate_path_relative_to should accept relative path in valid working_dir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert!(
        resolved.starts_with(temp_path),
        "Resolved path should be within working_dir: {:?} should start with {:?}",
        resolved,
        temp_path
    );
}

#[test]
fn test_validate_path_in_dir_accepts_outside_cwd() {
    // Arrange: use temp_dir() which is guaranteed to be outside CWD
    let temp_dir = std::env::temp_dir();
    let canonical_temp_dir =
        std::fs::canonicalize(&temp_dir).expect("should canonicalize temp_dir");

    // Act: call validate_path_relative_to with a relative filename
    let result = validate_path_relative_to("probe.txt", false, &temp_dir);

    // Assert: should accept working_dir outside CWD
    assert!(
        result.is_ok(),
        "validate_path_relative_to should accept working_dir outside CWD: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert!(
        resolved.starts_with(&canonical_temp_dir),
        "Resolved path should be within working_dir: {:?} should start with {:?}",
        resolved,
        canonical_temp_dir
    );
}

#[test]
fn test_edit_replace_with_working_dir() {
    // Arrange: create a temporary directory within CWD and file
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_path = temp_dir.path();
    let file_path = temp_path.join("test.txt");
    std::fs::write(&file_path, "hello world").expect("should write test file");

    // Act: call validate_path_relative_to with require_exists=true
    let result = validate_path_relative_to("test.txt", true, temp_path);

    // Assert: should find the file relative to working_dir
    assert!(
        result.is_ok(),
        "validate_path_relative_to should find existing file in working_dir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(
        resolved, file_path,
        "Resolved path should match the actual file path"
    );
}

#[test]
fn test_edit_overwrite_no_working_dir() {
    // Arrange: use validate_path without working_dir (existing behavior)
    // Use Cargo.toml which exists in the crate root

    // Act: call validate_path with require_exists=true
    let result = validate_path("Cargo.toml", true);

    // Assert: should work as before
    assert!(
        result.is_ok(),
        "validate_path should still work without working_dir"
    );
}

#[test]
fn test_edit_overwrite_working_dir_is_file() {
    // Arrange: create a temporary file (not directory) to use as working_dir
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_file = temp_dir.path().join("test_file.txt");
    std::fs::write(&temp_file, "test content").expect("should write test file");

    // Act: call validate_path_relative_to with a file as working_dir
    let result = validate_path_relative_to("some_file.txt", false, &temp_file);

    // Assert: should reject because working_dir is not a directory
    assert!(
        result.is_err(),
        "validate_path_relative_to should reject a file as working_dir"
    );
    let err = result.unwrap_err();
    let err_msg = err.message.to_lowercase();
    assert!(
        err_msg.contains("directory"),
        "Error message should mention 'directory': {}",
        err.message
    );
}

#[test]
fn test_validate_path_in_dir_traversal_to_sibling_accepted_with_working_dir() {
    // Arrange: create a parent temp dir, then two subdirs:
    //   allowed/          -- the working_dir
    //   allowed_sibling/  -- a sibling whose name shares the prefix
    //
    // With an explicit working_dir the containment boundary is the operator's
    // responsibility (MCP 2025-11-25 spec). validate_path_relative_to must NOT
    // reject a relative path that resolves outside working_dir -- the operator
    // chose the scope. CVE-2025-53110 (starts_with prefix attack) is a concern
    // only for validate_path (CWD-scoped), which still uses validate_parent_in_root.
    let cwd = std::env::current_dir().expect("should get cwd");
    let parent = tempfile::TempDir::new_in(&cwd).expect("should create parent temp dir");
    let allowed = parent.path().join("allowed");
    let sibling = parent.path().join("allowed_sibling");
    std::fs::create_dir_all(&allowed).expect("should create allowed dir");
    std::fs::create_dir_all(&sibling).expect("should create sibling dir");

    // Act: relative path that traverses from allowed/ into allowed_sibling/
    let result = validate_path_relative_to("../allowed_sibling/secret.txt", false, &allowed);

    // Assert: accepted -- the sibling dir exists and the operator set working_dir=allowed/
    assert!(
        result.is_ok(),
        "validate_path_relative_to must accept a path resolving outside working_dir; \
         containment is operator responsibility, not per-call: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    let canonical_sibling = std::fs::canonicalize(&sibling).expect("should canonicalize sibling");
    assert!(
        resolved.starts_with(&canonical_sibling),
        "Resolved path should be inside the sibling dir: {resolved:?}"
    );
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("secret.txt")
    );
}

#[test]
fn test_validate_path_in_dir_rejects_sibling_prefix_validate_path() {
    // CVE-2025-53110: validate_path (CWD-scoped, no working_dir) must reject a
    // path that resolves to a sibling directory sharing the CWD name prefix.
    // This is the original protection; validate_parent_in_root still enforces it.
    let result = validate_path("/etc/passwd", true);
    assert!(
        result.is_err(),
        "validate_path must reject absolute paths outside CWD"
    );
}

#[test]
fn test_validate_path_relative_to_accepts_absolute_path_with_working_dir() {
    // Arrange: create a temp dir to act as an external repo (e.g. career repo)
    // Use temp_dir() so it is guaranteed to be outside the server CWD.
    let external = tempfile::TempDir::new().expect("should create external temp dir");
    let canonical_external =
        std::fs::canonicalize(external.path()).expect("should canonicalize external dir");

    // Construct an absolute path to a (non-existent) file inside the external dir.
    let abs_path = canonical_external.join("AGENTS.md");
    let abs_path_str = abs_path.to_str().expect("should be valid UTF-8");

    // Act: provide the absolute path with working_dir set to the external dir.
    // This is the primary use case: agent passes absolute path + matching working_dir
    // to write a file in a repo that is outside the MCP server CWD.
    let result =
        validate_path_relative_to(abs_path_str, false, &canonical_external);

    // Assert: must succeed and resolve to the absolute path.
    assert!(
        result.is_ok(),
        "validate_path_relative_to must accept an absolute path when working_dir is set: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(
        resolved, abs_path,
        "Resolved path must match the supplied absolute path"
    );
}

#[test]
fn test_validate_path_in_dir_nonexistent_deep_path() {
    // Deeply nested non-existent path: a/b/c/d/new.txt -- none of the
    // intermediate directories exist.  With parent-directory validation,
    // this is rejected because the parent a/b/c/d does not exist.
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let result = validate_path_relative_to("a/b/c/d/new.txt", false, temp_dir.path());
    assert!(
        result.is_err(),
        "validate_path_relative_to should reject deeply nested non-existent path"
    );
}

#[test]
fn test_validate_path_in_dir_nonexistent_with_existing_parent() {
    // Partial existence: working_dir/sub/ exists but working_dir/sub/new.txt does not.
    // The loop should stop at sub/ (the first existing ancestor) and rejoin new.txt.
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let sub = temp_dir.path().join("sub");
    std::fs::create_dir_all(&sub).expect("should create sub dir");

    let result = validate_path_relative_to("sub/new.txt", false, temp_dir.path());
    assert!(
        result.is_ok(),
        "validate_path_relative_to should accept file in existing subdir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    let canonical_sub = std::fs::canonicalize(&sub).expect("should canonicalize sub");
    assert!(
        resolved.starts_with(&canonical_sub),
        "Resolved path should anchor at the existing sub/ dir: {resolved:?}"
    );
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("new.txt"),
        "File name component must be preserved"
    );
}
