"""Offline safety/accounting regressions for the opt-in live controllers."""

import json
from pathlib import Path
import tempfile
import unittest
from live_env import selected_values
from live_http import rpc_message
from live_support import reserve, write_json


class LiveToolTests(unittest.TestCase):
    def test_credentials_are_literal_selected_values(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env"
            path.write_text(
                'export SELECTED="literal-value"\nUNRELATED="unclosed\nSHELL=$(not-executed)\n'
            )
            self.assertEqual(
                selected_values(path, {"SELECTED", "SHELL"}),
                {"SELECTED": "literal-value", "SHELL": "$(not-executed)"},
            )

    def test_selected_invalid_quoting_has_no_value_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env"
            path.write_text("SELECTED='private-marker")
            with self.assertRaises(ValueError) as raised:
                selected_values(path, {"SELECTED"})
            self.assertNotIn("private-marker", str(raised.exception))

    def test_budget_ceiling_and_shared_context_reservation(self):
        with tempfile.TemporaryDirectory() as directory:
            campaign = Path(directory)
            write_json(
                campaign / "budget.json",
                {
                    "limit_usd": 25,
                    "reserved_usd": 0,
                    "provider_requests": 0,
                    "entries": [],
                },
            )
            reserve(campaign, "context", "first", 15, 150, once=True)
            reserve(campaign, "context", "continuation", 15, 150, once=True)
            reserve(campaign, "other", "call", 2, 1)
            with self.assertRaises(AssertionError):
                reserve(campaign, "other", "too-expensive", 9)
            for value in [-1, float("nan"), float("inf")]:
                with self.assertRaises(AssertionError):
                    reserve(campaign, "other", "invalid", value)
            ledger = json.loads((campaign / "budget.json").read_text())
            self.assertEqual(ledger["reserved_usd"], 17)
            self.assertEqual(ledger["provider_requests"], 151)

    def test_sse_empty_events_and_correlation(self):
        body = b'data:\r\n\r\n: heartbeat\r\n\r\ndata: {"jsonrpc":"2.0","id":1,"result":{}}\r\n\r\ndata: {"jsonrpc":"2.0","id":2,"result":{"tools":[]}}\r\n\r\n'
        self.assertEqual(rpc_message(body, "text/event-stream", 2), {"tools": []})
        with self.assertRaises(AssertionError):
            rpc_message(b'{"jsonrpc":"2.0","id":3,"result":{}}', "application/json", 2)
        with self.assertRaises(AssertionError):
            rpc_message(b"x" * (4 * 1024 * 1024 + 1), "application/json", 1)


if __name__ == "__main__":
    unittest.main()
