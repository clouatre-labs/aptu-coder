#!/usr/bin/env python3
"""Mechanical extraction of lib.rs tool handlers into tools/ submodule.
Run from worktree root: python3 scripts/extract_tools.py
"""
import os, re

SRC = "crates/aptu-coder/src"
LIB = os.path.join(SRC, "lib.rs")

with open(LIB) as f:
    orig = f.read()
lines = orig.splitlines(keepends=True)
N = len(lines)

def sec(start, end):
    return "".join(lines[start-1:end])

def find(pat, start=1, end=None):
    rx = re.compile(pat)
    for i in range(start-1, min(end or N, N)):
        if rx.search(lines[i]):
            return i+1
    return None

def trim_end(n):
    while n > 1 and lines[n-1].strip() == "":
        n -= 1
    return n

def struct_end(start):
    depth = 0
    for i in range(start-1, N):
        depth += lines[i].count('{') - lines[i].count('}')
        if depth == 0 and i >= start:
            return i+1
    return N

# ── Exact boundaries ───────────────────────────────────────────────────────────
TOOLS = ["analyze_directory", "analyze_file", "analyze_symbol",
         "analyze_module", "edit_overwrite", "edit_replace", "exec_command"]

TOOL_ATTR = {
    "analyze_directory": 1447, "analyze_file": 1658, "analyze_symbol": 1935,
    "analyze_module": 2380,   "edit_overwrite": 2654, "edit_replace": 2897,
    "exec_command": 3334,
}
TOOL_FN = {
    "analyze_directory": 1461, "analyze_file": 1672, "analyze_symbol": 1949,
    "analyze_module": 2394,   "edit_overwrite": 2668, "edit_replace": 2911,
    "exec_command": 3335,
}
TOOL_END = {   # closing } of each method body (verified by brace tracking)
    "analyze_directory": 1656, "analyze_file": 1933, "analyze_symbol": 2378,
    "analyze_module": 2652,   "edit_overwrite": 2895, "edit_replace": 3319,
    "exec_command": 3689,
}

TOOL_ROUTER  = 424;  TOOL_HANDLER = 4112;  CFG_TEST = 4401
BUILD_EXEC   = 3693; STRIP_CD     = 3734;  EXEC_RESULT = 3749
RUN_TIMEOUT  = 3759; RUN_EXEC     = 3882;  HANDLE_OUT  = 4010
FOCUSED_P    = 4092; DISABLE_RT   = 4106  # 4092 = #[derive(Clone)] before FocusedAnalysisParams
EMIT_PROG_A  = 538;  FIRST_TOOL_ATTRS = 1447

CA_STRUCT      = find(r"^pub struct CodeAnalyzer")
PRE_FNS_START  = 197
PRE_FNS_END    = trim_end(CA_STRUCT - 1)
EXT_IMPL_START = find(r"^impl<'a> opentelemetry::propagation::Extractor for ExtractMap")
EXT_IMPL_END   = struct_end(EXT_IMPL_START)
FP_END         = struct_end(FOCUSED_P)
ER_END         = struct_end(EXEC_RESULT)
NEW_END        = trim_end(EMIT_PROG_A - 1)
NEW_FN_LN      = find(r"^\s+pub fn new\(", start=TOOL_ROUTER, end=FIRST_TOOL_ATTRS)
CA_STRUCT_ATTRS= find(r"^/// MCP server handler", start=CA_STRUCT-10, end=CA_STRUCT) or CA_STRUCT
SIZE_LIMIT_LN  = find(r"^const SIZE_LIMIT", start=190, end=210)
DRAIN_LN       = find(r"^const DEFAULT_DRAIN_TIMEOUT_MS", start=40, end=70)

print(f"CA_STRUCT={CA_STRUCT} EXT_IMPL_END={EXT_IMPL_END}")
print(f"FP_END={FP_END} ER_END={ER_END} NEW_END={NEW_END} NEW_FN_LN={NEW_FN_LN}")

# ── Create tools/ ──────────────────────────────────────────────────────────────
os.makedirs(os.path.join(SRC, "tools"), exist_ok=True)

with open(os.path.join(SRC, "tools", "mod.rs"), "w") as f:
    f.write("pub(crate) mod common;\n")
    for t in TOOLS:
        f.write(f"pub(crate) mod {t};\n")
print("Wrote tools/mod.rs")

# ── Shared preamble for all tool modules ───────────────────────────────────────
# This includes all imports the tool bodies need, with paths correct for tools/
# sub-module (crate:: prefix where lib.rs uses bare names).
SHARED_PREAMBLE = """\
#![allow(unused_imports)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::cast_precision_loss)]
use aptu_coder_core::analyze;
use aptu_coder_core::{cache, completion, graph, traversal, types};
use aptu_coder_core::cache::{AnalysisCache, CacheTier, CallGraphCache, CallGraphCacheKey};
use aptu_coder_core::formatter::{
    format_file_details_paginated, format_file_details_summary, format_focused_paginated,
    format_module_info, format_structure_paginated, format_summary,
};
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::parser::ParserError;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{
    AnalysisMode, AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeModuleParams,
    AnalyzeSymbolParams, EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput,
    EditReplaceParams, SymbolMatchMode,
};
use crate::filters::{CompiledRule, apply_filter, load_filter_table, maybe_inject_no_stat};
use crate::logging::LogEvent;
use crate::shell::resolve_shell;
use crate::validation::{validate_path, validate_path_in_dir};
use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    LoggingLevel, LoggingMessageNotificationParam, Meta, Notification, ProgressNotificationParam,
    ProgressToken, ServerCapabilities, ServerNotification, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc, watch};
use tracing::{instrument, warn};
use tracing_subscriber::filter::LevelFilter;
"""

# ── tools/common.rs ────────────────────────────────────────────────────────────
pre_fns = sec(PRE_FNS_START, PRE_FNS_END)
for fn_name in ["error_meta", "err_to_tool_result", "err_to_tool_result_from_pagination",
                "no_cache_meta", "paginate_focus_chains"]:
    pre_fns = re.sub(rf'^fn {fn_name}\b', f'pub(crate) fn {fn_name}', pre_fns, flags=re.MULTILINE)

def make_pub_crate_struct(text, struct_name):
    text = text.replace(f"struct {struct_name}", f"pub(crate) struct {struct_name}", 1)
    result = []
    depth = 0
    in_body = False
    for line in text.splitlines(keepends=True):
        depth += line.count('{') - line.count('}')
        stripped = line.strip()
        if '{' in line and not in_body:
            in_body = True
            result.append(line)
            continue
        if in_body and depth > 0 and stripped and not stripped.startswith('//') \
                and not stripped.startswith('#') and ':' in stripped \
                and not stripped.startswith('pub'):
            result.append(re.sub(r'^(\s+)(\w)', r'\1pub(crate) \2', line))
        else:
            result.append(line)
    return ''.join(result)

fp_text = make_pub_crate_struct(sec(FOCUSED_P, FP_END), "FocusedAnalysisParams")
er_text = make_pub_crate_struct(sec(EXEC_RESULT, ER_END), "ExecutionResult")

helpers_text = sec(EMIT_PROG_A, trim_end(FIRST_TOOL_ATTRS - 1))
for method in ["emit_progress", "emit_received_metric", "handle_overview_mode",
               "poll_progress_until_done", "run_focused_with_auto_summary",
               "handle_focused_mode", "validate_impl_only", "validate_import_lookup",
               "handle_file_details_mode"]:
    helpers_text = re.sub(
        r'^(\s+)(async fn |fn )(' + method + r'\b)',
        r'\1pub(crate) \2\3', helpers_text, flags=re.MULTILINE
    )

def range_text(start, end):
    return sec(start, trim_end(end)) + "\n\n"

post_fns_raw = (
    range_text(BUILD_EXEC,  STRIP_CD - 1) +
    range_text(STRIP_CD,    EXEC_RESULT - 1) +
    range_text(RUN_TIMEOUT, RUN_EXEC - 1) +
    range_text(RUN_EXEC,    HANDLE_OUT - 1) +
    range_text(HANDLE_OUT,  FOCUSED_P - 1) +
    range_text(DISABLE_RT,  TOOL_HANDLER - 1)
)
for fn_name in ["build_exec_command", "strip_cd_prefix", "run_with_timeout",
                "run_exec_impl", "handle_output_persist", "disable_routes"]:
    post_fns_raw = re.sub(
        rf'^(?:async fn |fn ){fn_name}\b',
        lambda m, n=fn_name: 'pub(crate) ' + m.group(0),
        post_fns_raw, flags=re.MULTILINE
    )

# Constants that tools need (SIZE_LIMIT, DEFAULT_DRAIN_TIMEOUT_MS)
consts = ""
if SIZE_LIMIT_LN:
    c = sec(SIZE_LIMIT_LN, SIZE_LIMIT_LN).replace("const SIZE_LIMIT", "pub(crate) const SIZE_LIMIT")
    consts += c
if DRAIN_LN:
    c = sec(DRAIN_LN, DRAIN_LN).replace("const DEFAULT_DRAIN_TIMEOUT_MS", "pub(crate) const DEFAULT_DRAIN_TIMEOUT_MS")
    consts += c

# ShellOutput is used in run_exec_impl -- add pub(crate) re-export
SHELL_OUTPUT_LINE = find(r"^pub struct ShellOutput")
SHELL_OUTPUT_IMPL = find(r"^impl ShellOutput")
SHELL_OUTPUT_END  = struct_end(SHELL_OUTPUT_IMPL)
shell_output_text = sec(SHELL_OUTPUT_LINE, SHELL_OUTPUT_END)

common = (
    SHARED_PREAMBLE
    + "use crate::CodeAnalyzer;\n"
    + "use crate::{ExecCommandParams, ShellOutput, EDIT_FAILURE_MAP_CAP, EDIT_STALE_THRESHOLD, STDIN_MAX_BYTES};\n\n"
    + consts + "\n"
    + pre_fns + "\n\n"
    + fp_text + "\n\n"
    + er_text + "\n\n"
    + "impl CodeAnalyzer {\n"
    + helpers_text
    + "}\n\n"
    + post_fns_raw
)

with open(os.path.join(SRC, "tools", "common.rs"), "w") as f:
    f.write(common)
print("Wrote tools/common.rs")

# ── Tool files ─────────────────────────────────────────────────────────────────
TOOL_EXTRA_USES = {
    "analyze_directory": "",
    "analyze_file":      "",
    "analyze_symbol":    "",
    "analyze_module":    "",
    "edit_overwrite":    "use crate::{EDIT_FAILURE_MAP_CAP, EDIT_STALE_THRESHOLD};\n",
    "edit_replace":      "use crate::{EDIT_FAILURE_MAP_CAP, EDIT_STALE_THRESHOLD};\n",
    "exec_command":
        "use crate::{ExecCommandParams, STDIN_MAX_BYTES};\n"
        "use crate::tools::common::{ExecutionResult, DEFAULT_DRAIN_TIMEOUT_MS, "
        "build_exec_command, handle_output_persist, run_exec_impl, run_with_timeout, strip_cd_prefix};\n",
}

TOOL_COMMON_USES = (
    "use crate::CodeAnalyzer;\n"
    "use crate::tools::common::{ClientMetadata, FocusedAnalysisParams, SIZE_LIMIT,\n"
    "    error_meta, err_to_tool_result, err_to_tool_result_from_pagination,\n"
    "    extract_and_set_trace_context, no_cache_meta, paginate_focus_chains,\n"
    "    summary_cursor_conflict, disable_routes};\n"
)

for tool in TOOLS:
    fn_line  = TOOL_FN[tool]
    end_line = TOOL_END[tool]
    body = sec(fn_line, end_line)

    # Replace method signature -> free fn
    body = re.sub(
        r'^\s+(?:pub )?async fn ' + tool + r'\(',
        'pub(crate) async fn ' + tool + '_impl(',
        body, flags=re.MULTILINE, count=1,
    )
    body = re.sub(r'^\s{8}&self,\s*$', '    analyzer: &CodeAnalyzer,', body,
                  flags=re.MULTILINE, count=1)
    body = re.sub(r'^        (params: Parameters)', r'    \1', body, flags=re.MULTILINE)
    body = re.sub(r'^        (context: RequestContext)', r'    \1', body, flags=re.MULTILINE)
    body = re.sub(r'^    \) -> Result<CallToolResult, ErrorData> \{',
                  r') -> Result<CallToolResult, ErrorData> {', body, flags=re.MULTILINE)
    # Replace self. with analyzer. -- but NOT in "Self::" (type-level) patterns
    body = re.sub(r'\bself\.', 'analyzer.', body)
    # Also replace "self\n    ." chains (multi-line method chaining)
    body = re.sub(r'\bself\n(\s+)\.', r'analyzer\n\1.', body)
    # Replace Self:: with CodeAnalyzer:: (free fn context)
    body = body.replace('Self::', 'CodeAnalyzer::')
    # Fix bare validation:: -> crate::validation::
    body = re.sub(r'\bvalidation::', 'crate::validation::', body)
    # De-indent 4 spaces (was inside impl block)
    result_lines = []
    for ln in body.splitlines(keepends=True):
        result_lines.append(ln[4:] if ln.startswith('    ') else ln)
    body = ''.join(result_lines)

    content = SHARED_PREAMBLE + TOOL_COMMON_USES + TOOL_EXTRA_USES[tool] + "\n" + body + "\n"
    path = os.path.join(SRC, "tools", f"{tool}.rs")
    with open(path, "w") as f:
        f.write(content)
    print(f"Wrote tools/{tool}.rs")

# ── Rewrite lib.rs ─────────────────────────────────────────────────────────────
def tool_stub(tool):
    a = TOOL_ATTR[tool]
    f = TOOL_FN[tool]
    attrs = sec(a, f - 1)
    sig = sec(f, f + 4).rstrip()
    if sig.endswith('{'):
        sig = sig[:-1].rstrip()
    return (attrs + sig + " {\n"
            + f"        tools::{tool}::{tool}_impl(self, params, context).await\n"
            + "    }\n")

new_lib = (
    sec(1, EXT_IMPL_END) + "\n"
    + "mod tools;\n\n"
    + sec(CA_STRUCT_ATTRS, trim_end(TOOL_ROUTER - 1)) + "\n"
    + "#[tool_router]\nimpl CodeAnalyzer {\n"
    + "    #[must_use]\n"
    + "    pub fn list_tools() -> Vec<rmcp::model::Tool> {\n"
    + "        Self::tool_router().list_all()\n"
    + "    }\n\n"
    + sec(NEW_FN_LN, NEW_END) + "\n"
    + "\n".join(tool_stub(t) for t in TOOLS) + "\n"
    + "}\n\n"
    + sec(TOOL_HANDLER, trim_end(CFG_TEST - 1)) + "\n"
    + sec(CFG_TEST, N)
)

with open(LIB, "w") as f:
    f.write(new_lib)

cfg_new = next(i+1 for i, ln in enumerate(new_lib.splitlines()) if ln.startswith("#[cfg(test"))
total_lines = len(new_lib.splitlines())
print(f"Wrote lib.rs: {total_lines} total, {cfg_new-1} non-test lines")
print("Done!")
