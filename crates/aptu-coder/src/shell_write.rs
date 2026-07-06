// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shell command validation helpers: heredoc and file-write pattern detection.
//!
//! Pre-spawn exec_command guard: rejects heredoc patterns before any process
//! is spawned.  Validation logic is split across:
//!
//! - [`heredoc_validation`]: [`validate_heredocs`] and its error helpers.
//! - [`shell_scan`]: backward token-scanning primitives.
//!
//! This module re-exports the public API for backward compatibility with
//! `exec_command.rs` which calls `crate::shell_write::validate_heredocs`.

pub(crate) use crate::heredoc_validation::validate_heredocs;
