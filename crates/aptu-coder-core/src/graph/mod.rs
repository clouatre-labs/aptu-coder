// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

pub mod call_graph;
pub mod store;
pub mod structural;

#[rustfmt::skip]
pub use call_graph::{CallGraph, InternalCallChain, GraphError, resolve_symbol};
