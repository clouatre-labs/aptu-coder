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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn test_resolve_shell_aptu_shell_env_takes_priority() {
        // Arrange: set APTU_SHELL to an explicit value
        // SAFETY: serial_test::serial serializes all tests in this module that
        // mutate APTU_SHELL, preventing concurrent env var races.
        unsafe { std::env::set_var("APTU_SHELL", "zsh") };

        // Act
        let shell = resolve_shell();

        // Cleanup before assertions to minimise env pollution window
        unsafe { std::env::remove_var("APTU_SHELL") };

        // Assert: APTU_SHELL wins over PATH-based detection
        assert_eq!(
            shell, "zsh",
            "APTU_SHELL must take priority over PATH detection"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn test_resolve_shell_falls_back_when_path_empty() {
        // Arrange: remove APTU_SHELL and set PATH to empty so bash is not found
        // SAFETY: serial_test::serial serializes all tests in this module that
        // mutate PATH/APTU_SHELL, preventing concurrent env var races.
        unsafe { std::env::remove_var("APTU_SHELL") };
        let saved_path = std::env::var("PATH").ok();
        unsafe { std::env::set_var("PATH", "") };

        // Act
        let shell = resolve_shell();

        // Restore PATH before assertions
        match saved_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        // Assert: falls back to /bin/sh when bash is not on PATH
        assert_eq!(
            shell, "/bin/sh",
            "must fall back to /bin/sh when bash is not on PATH"
        );
    }
}
