import sys
from pathlib import Path
import base64
import json
import os
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from muse_bridge import ManagedRpc, normalize, final_report
from muse_transport import Broker
import muse_transport


class ProtocolTests(unittest.TestCase):
    def test_account_discovery_reads_only_status_without_credentials_or_inference(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
            "HOME": directory, "PATH": "/usr/bin", "XDG_CONFIG_HOME": directory + "/native",
            "META_API_KEY": "synthetic-secret", "TYPESAFE_API_KEY": "synthetic-secret",
        }, clear=True), patch("muse_transport.Rpc") as client:
            root = Path(directory) / "check"
            rpc = client.return_value
            rpc.call.side_effect = [{}, {"state": "accountLogin", "account": "private-account"}]
            self.assertEqual(muse_transport.read_account(root), {"state": "accountLogin"})
            self.assertEqual([call.args[0] for call in rpc.call.call_args_list], ["initialize", "account/read"])
            rpc.close.assert_called_once()
            command, env, cwd, own_group = client.call_args.args
            self.assertEqual(command, ["muse", "serve", "--disable-shell", "--disable-write"])
            self.assertFalse(own_group)
            self.assertEqual(cwd, root)
            self.assertNotIn("META_API_KEY", env)
            self.assertNotIn("TYPESAFE_API_KEY", env)
            auth = root / "config/muse/auth.json"
            self.assertEqual(auth.readlink(), Path(directory) / "native/muse/auth.json")
            settings = json.loads((root / "config/muse/settings.json").read_text())
            self.assertEqual(settings["endpoint_transport"]["base_url"], "http://127.0.0.1:9")
            self.assertEqual(settings["run"]["toolset"], [])

    def test_account_failure_closes_its_native_cli(self):
        with patch("muse_transport.account_client") as client:
            client.return_value.call.side_effect = [{}, RuntimeError("Account unavailable")]
            with self.assertRaises(RuntimeError):
                muse_transport.read_account(Path("unused"))
            client.return_value.close.assert_called_once()

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
