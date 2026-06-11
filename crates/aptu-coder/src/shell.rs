// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shell detection helper used by the exec_command tool handler.

/// Resolve the preferred shell for command execution.
/// Priority: APTU_SHELL env var > bash (PATH search) > /bin/sh (unix) / cmd (windows).
/// APTU_SHELL is honored on all platforms so callers can override the shell uniformly.
pub(crate) fn resolve_shell() -> String {
    if let Ok(shell) = std::env::var("APTU_SHELL") {
        return shell;
    }
    #[cfg(unix)]
    {
        if std::env::var("PATH").is_ok_and(|p| {
            std::env::split_paths(&p).any(|dir| {
                use std::os::unix::fs::PermissionsExt as _;
                let candidate = dir.join("bash");
                candidate.is_file()
                    && candidate
                        .metadata()
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
            })
        }) {
            return "bash".to_string();
        }
        "/bin/sh".to_string()
    }
    #[cfg(not(unix))]
    {
        "cmd".to_string()
    }
}
