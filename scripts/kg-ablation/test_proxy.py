"""Unit tests for the MCP visibility proxy (surface-hiding).

Run: python3 -m unittest scripts/kg-ablation/test_proxy.py
(split: the package import path is exercised via sys.path bootstrap)
"""

import importlib.util
import json
import sys
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "kg_proxy",
    Path(__file__).resolve().parent / "proxy.py")
kg_proxy = importlib.util.module_from_spec(_SPEC)
sys.modules["kg_proxy"] = kg_proxy
_SPEC.loader.exec_module(kg_proxy)


def tools_list_response():
    return {
        "jsonrpc": "2.0", "id": 1,
        "result": {"tools": [
            {"name": "analyze_symbol", "description": "graph tool"},
            {"name": "aptu-coder_analyze_file", "description": "file tool"},
            {"name": "aptu-coder_analyze_directory", "description": "dir"},
        ]},
    }


class TestNoGraphArm(unittest.TestCase):
    """Design: tools/list omits analyze_symbol; graph methods rejected."""

    def setUp(self):
        self.p = kg_proxy.Proxy("no-graph", ["true"], lambda d, m: None)
        # Register the pending request as the proxy would on forwarding.
        self.p.pending[1] = "tools/list"

    def test_tools_list_omits_analyze_symbol(self):
        out = self.p.filter_response(tools_list_response())
        names = [t["name"] for t in out["result"]["tools"]]
        self.assertNotIn("analyze_symbol", names)
        self.assertIn("aptu-coder_analyze_file", names)

    def test_resources_list_rejected(self):
        req = {"jsonrpc": "2.0", "id": 7, "method": "resources/list",
               "params": {}}
        out = self.p.reject(req)
        self.assertIsNotNone(out)
        self.assertEqual(out["id"], 7)
        self.assertIn("error", out)
        self.assertNotIn("result", out)

    def test_resources_templates_list_rejected(self):
        req = {"jsonrpc": "2.0", "id": 8,
               "method": "resources/templates/list", "params": {}}
        out = self.p.reject(req)
        self.assertIsNotNone(out)
        self.assertIn("error", out)

    def test_graph_resource_read_rejected(self):
        req = {"jsonrpc": "2.0", "id": 9, "method": "resources/read",
               "params": {"uri": "aptu-coder://graph/abc/blast-radius/foo"}}
        out = self.p.reject(req)
        self.assertIsNotNone(out)
        self.assertIn("error", out)
        self.assertIn("disabled", out["error"]["message"])

    def test_nongraph_resource_read_passes(self):
        req = {"jsonrpc": "2.0", "id": 10, "method": "resources/read",
               "params": {"uri": "aptu-overflow://1/page/0"}}
        out = self.p.reject(req)
        self.assertIsNone(out)

    def test_analyze_symbol_call_rejected(self):
        req = {"jsonrpc": "2.0", "id": 11, "method": "tools/call",
               "params": {"name": "analyze_symbol", "arguments": {}}}
        out = self.p.reject(req)
        self.assertIsNotNone(out)
        self.assertIn("error", out)

    def test_regular_tool_call_passes(self):
        req = {"jsonrpc": "2.0", "id": 12, "method": "tools/call",
               "params": {"name": "aptu-coder_analyze_file",
                          "arguments": {}}}
        self.assertIsNone(self.p.reject(req))

    def test_error_is_clean_jsonrpc(self):
        req = {"jsonrpc": "2.0", "id": 13, "method": "resources/list"}
        out = self.p.reject(req)
        self.assertEqual(out["jsonrpc"], "2.0")
        self.assertIsInstance(out["error"]["code"], int)
        self.assertIsInstance(out["error"]["message"], str)


class TestOptionalAndForcedArms(unittest.TestCase):
    """Design: non-graph tools and resources pass through unchanged."""

    def test_optional_passthrough_unchanged(self):
        p = kg_proxy.Proxy("optional-graph", ["true"], lambda d, m: None)
        p.pending[1] = "tools/list"
        resp = tools_list_response()
        self.assertEqual(p.filter_response(resp), resp)
        self.assertIsNone(p.reject({"jsonrpc": "2.0", "id": 2,
                                    "method": "resources/list"}))
        self.assertIsNone(p.reject({"jsonrpc": "2.0", "id": 3,
                                    "method": "resources/read",
                                    "params": {"uri":
                                               "aptu-coder://graph/x/y/z"}}))

    def test_forced_passthrough_unchanged(self):
        p = kg_proxy.Proxy("forced-graph", ["true"], lambda d, m: None)
        p.pending[1] = "tools/list"
        resp = tools_list_response()
        self.assertEqual(p.filter_response(resp), resp)
        self.assertIsNone(p.reject({"jsonrpc": "2.0", "id": 4,
                                    "method": "resources/list"}))

    def test_forced_injects_preamble_after_initialize(self):
        p = kg_proxy.Proxy("forced-graph", ["true"], lambda d, m: None)
        p.pending[5] = "initialize"
        init = {"jsonrpc": "2.0", "id": 5, "result": {"ok": True}}
        extra = p.maybe_preamble(init)
        self.assertIsNotNone(extra)
        self.assertEqual(extra["method"], "notifications/message")
        self.assertIn("analyze_symbol", extra["params"]["data"])
        # Exactly once.
        self.assertIsNone(p.maybe_preamble(init))


class TestAdjudicator(unittest.TestCase):
    """Rubric classification matches the design definitions."""

    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location(
            "kg_adjudicate",
            Path(__file__).resolve().parent / "adjudicate.py")
        cls.adj = importlib.util.module_from_spec(spec)
        sys.modules["kg_adjudicate"] = cls.adj
        spec.loader.exec_module(cls.adj)

    def test_named_defect_catch(self):
        text = ("The function validate_path_relative_to now requires a "
                "third argument, but verify_anchors.rs still calls it "
                "with the old signature; this is a stale caller that "
                "will fail to compile.")
        out = self.adj.classify(text, "broken-caller")
        self.assertTrue(out["catch"])
        self.assertFalse(out["false_negative"])

    def test_generic_hedge_is_not_a_catch(self):
        text = ("The change looks reasonable. You should check all "
                "callers of changed functions in case something might "
                "break elsewhere.")
        out = self.adj.classify(text, "broken-caller")
        self.assertFalse(out["catch"])

    def test_false_negative_when_silent(self):
        out = self.adj.classify("The diff adds a helpful doc comment. "
                                "Looks good overall.", "dead-code-path")
        self.assertTrue(out["false_negative"])
        self.assertFalse(out["catch"])

    def test_clean_control_false_positive(self):
        text = ("This diff introduces a security vulnerability: it "
                "handles user credentials without sanitization and "
                "could leak secrets.")
        out = self.adj.classify(text, "clean-control")
        self.assertTrue(out["false_positive"])

    def test_clean_control_clean_pass(self):
        out = self.adj.classify("Harmless comment rename; no functional "
                                "change detected.", "clean-control")
        self.assertFalse(out["false_positive"])
        self.assertFalse(out["catch"])


class TestConfig(unittest.TestCase):
    def test_matrix_is_24_cells(self):
        spec = importlib.util.spec_from_file_location(
            "kg_config",
            Path(__file__).resolve().parent / "config.py")
        cfg = importlib.util.module_from_spec(spec)
        sys.modules["kg_config"] = cfg
        spec.loader.exec_module(cfg)
        matrix = cfg.session_matrix()
        self.assertEqual(len(matrix), 24)
        self.assertEqual(len(set(matrix)), 24)
        self.assertEqual(cfg.SPEND_CAP_USD, 0.069)


if __name__ == "__main__":
    unittest.main()
