import sys
from pathlib import Path
import base64
import json
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from muse_bridge import ManagedRpc, normalize, final_report
from muse_transport import Broker


class ProtocolTests(unittest.TestCase):
    def test_reports_and_commentary_keep_distinct_roles(self):
        commentary = normalize({"method": "item/completed", "params": {"item": {
            "kind": "agentMessage", "itemId": "one", "text": "Inspecting source."}}})
        self.assertEqual(commentary["params"]["item"]["phase"], "commentary")
        text = '{"status":"completed","summary":"Done","checks":[]}'
        report = normalize({"method": "item/completed", "params": {"item": {
            "kind": "agentMessage", "id": "two", "text": text}}})
        self.assertEqual(report["params"]["item"]["phase"], "final_answer")
        self.assertEqual(report["params"]["item"]["id"], "two")
        self.assertFalse(final_report("Done"))
        self.assertFalse(final_report("[]"))

    def test_interrupted_turns_cannot_look_completed(self):
        event = normalize({"method": "turn/completed", "params": {
            "turnId": "turn-1", "terminal": "interrupted"}})
        self.assertEqual(event["params"]["turn"], {"id": "turn-1", "status": "interrupted"})
        self.assertIsNone(normalize({"method": "item/completed", "params": {
            "item": {"kind": "toolCall", "status": "completed"}}}))

    def test_mcp_cannot_call_unadvertised_tools_or_forward_invalid_arguments(self):
        for params in [[], {"name": []}, {"name": "publish_pr", "arguments": {}},
                       {"name": "read_worker_messages", "arguments": "other-worker"}]:
            bridge = object.__new__(ManagedRpc)
            bridge.broker = Broker({"authorization": "Bearer synthetic-provider-secret"})
            bridge.tools = [{"name": "read_worker_messages"}]
            bridge.stopped = False
            bridge.pool = threading.BoundedSemaphore(1)
            bridge.pool.acquire()
            replies, calls = [], []
            bridge.envelope, bridge.emit = replies.append, calls.append
            bridge.relay({"id": "request", "path": "/sprowt/tools", "method": "POST",
                "authorization": "Bearer " + bridge.broker.capability,
                "body": base64.b64encode(json.dumps(params).encode()).decode()})
            self.assertFalse(calls)
            self.assertEqual(replies[-1]["status"], 502)


if __name__ == "__main__":
    unittest.main()
