#!/usr/bin/env python3
"""Stdio MCP visibility proxy for the KG pull-ablation benchmark.

Sits between the MCP client (pi via pi-mcp-adapter) and the real
aptu-coder stdio MCP server. Does not modify aptu-coder: it only changes
the client-visible surface per arm.

Arms:
- no-graph:      strips `analyze_symbol` from tools/list responses; rejects
                 resources/list, resources/templates/list, graph
                 resources/read, and analyze_symbol tools/call with clean
                 JSON-RPC errors. The real no-graph arm from the design.
- optional-graph: full passthrough.
- forced-graph:  full passthrough plus a fixed graph-workflow preamble
                 injected as a logging notification after initialize.
                 (Availability upper bound; the runner prompt repeats the
                 same pinned instruction.)
"""

from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import sys
import threading

METHOD_NOT_FOUND = -32601
GRAPH_DISABLED_MSG = "graph surface disabled in this benchmark arm"

REJECTED_METHODS = frozenset({"resources/list", "resources/templates/list"})
HIDDEN_TOOL = "analyze_symbol"
GRAPH_URI_PREFIX = "aptu-coder://graph/"

FORCED_PREAMBLE = (
    "[kg-ablation forced-graph preamble] Before reviewing, call the "
    "analyze_symbol tool on the primary changed symbol and read the "
    "graph resources relevant to the change, incorporating all returned "
    "pages into your analysis."
)


class Proxy:
    def __init__(self, arm: str, server_cmd: list[str], log):
        if arm not in ("no-graph", "optional-graph", "forced-graph"):
            raise ValueError(f"unknown arm: {arm}")
        self.arm = arm
        self.server_cmd = server_cmd
        self.log = log  # callable(direction, obj)
        self.pending: dict[object, str] = {}  # request id -> method
        self.lock = threading.Lock()
        self.preamble_sent = False

    # -- arm policy -----------------------------------------------------

    def reject(self, msg: dict) -> dict | None:
        """Return a clean JSON-RPC error response if the request must be
        rejected by the no-graph arm, else None."""
        if self.arm != "no-graph":
            return None
        method = msg.get("method")
        rid = msg.get("id")
        if method in REJECTED_METHODS:
            return self._err(rid, METHOD_NOT_FOUND,
                             f"{method}: {GRAPH_DISABLED_MSG}")
        if method == "tools/call":
            params = msg.get("params") or {}
            if params.get("name") == HIDDEN_TOOL:
                return self._err(rid, METHOD_NOT_FOUND,
                                 f"tool {HIDDEN_TOOL}: {GRAPH_DISABLED_MSG}")
        if method == "resources/read":
            uri = (msg.get("params") or {}).get("uri", "")
            if uri.startswith(GRAPH_URI_PREFIX):
                return self._err(rid, METHOD_NOT_FOUND,
                                 f"resource {uri}: {GRAPH_DISABLED_MSG}")
        return None

    @staticmethod
    def _err(rid: object, code: int, message: str) -> dict:
        return {"jsonrpc": "2.0", "id": rid,
                "error": {"code": code, "message": message}}

    def filter_response(self, msg: dict) -> dict:
        """Strip the hidden tool from tools/list results (no-graph)."""
        if self.arm != "no-graph":
            return msg
        rid = msg.get("id")
        if rid is not None and self.pending.get(rid) == "tools/list":
            result = msg.get("result")
            if isinstance(result, dict) and isinstance(result.get("tools"), list):
                result["tools"] = [
                    t for t in result["tools"]
                    if t.get("name") != HIDDEN_TOOL
                ]
        return msg

    def maybe_preamble(self, msg: dict) -> dict | None:
        """In the forced arm, inject the fixed preamble notification once,
        right after the initialize result passes through."""
        if self.arm != "forced-graph" or self.preamble_sent:
            return None
        if msg.get("id") is not None and \
                self.pending.get(msg.get("id")) == "initialize" \
                and "result" in msg:
            self.preamble_sent = True
            return {
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {
                    "level": "notice",
                    "data": FORCED_PREAMBLE,
                },
            }
        return None

    # -- framing --------------------------------------------------------

    def write_msg(self, w, msg: dict) -> None:
        w.write((json.dumps(msg) + "\n").encode("utf-8"))
        w.flush()

    def run(self) -> int:
        proc = subprocess.Popen(
            self.server_cmd,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        assert proc.stdin is not None and proc.stdout is not None
        sel = selectors.DefaultSelector()
        client_in = sys.stdin.buffer
        sel.register(client_in, selectors.EVENT_READ, "client")
        sel.register(proc.stdout, selectors.EVENT_READ, "server")
        try:
            while True:
                for key, _ in sel.select():
                    src = key.data
                    line = (client_in if src == "client" else proc.stdout
                            ).readline()
                    if not line:
                        return proc.wait() & 0xFF
                    try:
                        msg = json.loads(line.decode("utf-8"))
                    except (UnicodeDecodeError, json.JSONDecodeError):
                        continue
                    self.log(f"{src}->server" if src == "client"
                             else f"server->{src}", msg)
                    if src == "client":
                        if not isinstance(msg, dict) or "method" not in msg:
                            continue
                        if msg.get("id") is not None:
                            with self.lock:
                                self.pending[msg["id"]] = msg["method"]
                        reject = self.reject(msg)
                        if reject is not None:
                            self.log("proxy-reject", reject)
                            self.write_msg(client_out(reject), reject)
                            continue
                        self.write_msg(proc.stdin, msg)
                    else:
                        if not isinstance(msg, dict):
                            continue
                        out = self.filter_response(msg)
                        extra = self.maybe_preamble(out)
                        self.write_msg(client_out(out), out)
                        if extra is not None:
                            self.log("proxy-inject", extra)
                            self.write_msg(client_out(extra), extra)
                        with self.lock:
                            self.pending.pop(msg.get("id"), None)
        finally:
            try:
                proc.kill()
            except OSError:
                pass


def client_out(msg: dict):
    """stdout buffer of the proxy process (client-facing side)."""
    return sys.stdout.buffer


def open_log(log_dir: str | None):
    handle = None

    def log(direction: str, obj: dict) -> None:
        nonlocal handle
        if log_dir is None:
            return
        if handle is None:
            handle = open(  # noqa: SIM115
                os.path.join(log_dir, "mcp.jsonl"), "a", encoding="utf-8"
            )
        handle.write(json.dumps({"dir": direction, "msg": obj}) + "\n")
        handle.flush()

    return log


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--arm", required=True,
                    choices=["no-graph", "optional-graph", "forced-graph"])
    ap.add_argument("--server", nargs="+", default=["aptu-coder"],
                    help="stdio MCP server command to proxy")
    ap.add_argument("--log-dir", default=None)
    args = ap.parse_args()
    return Proxy(args.arm, args.server, open_log(args.log_dir)).run()


if __name__ == "__main__":
    sys.exit(main())
