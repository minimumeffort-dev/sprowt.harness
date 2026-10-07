import sys
from pathlib import Path
import base64
import json
import os
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from muse_bridge import ManagedRpc, SessionState, connect, recovered_thread, normalize, final_report
from muse_transport import Broker
import muse_transport


class ProtocolTests(unittest.TestCase):
    def test_delivery_ids_are_saved_before_submission_and_survive_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "muse/2.json"
            config = {"name": "fixture-vm", "cwd": "/tasks/7"}
            state = SessionState(path, config)
            command = state.command("instruction-1")
            self.assertNotIn("turnId", json.loads(path.read_text())["commands"]["instruction-1"])
            reloaded = SessionState(path, config)
            self.assertEqual(reloaded.command("instruction-1"), command)
            self.assertEqual(reloaded.value["sessionId"], state.value["sessionId"])
            reloaded.accepted("instruction-1", {"status": "accepted", "turnId": "native-turn"})
            self.assertEqual(SessionState(path, config).value["commands"]["instruction-1"]["turnId"], "native-turn")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            next_task = SessionState(path, {**config, "cwd": "/tasks/8"})
            self.assertNotEqual(next_task.value["sessionId"], state.value["sessionId"])
            self.assertEqual(next_task.value["commands"], {})

    def test_native_receipts_recover_missing_acks_and_completed_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            state = SessionState(Path(directory) / "state.json", {"name": "vm", "cwd": "/tasks/7"})
            command = state.command("task-source")
            steering = state.command("steering-source")
            report = '{"status":"completed","summary":"Done","checks":[]}'
            items = [
                {"kind": "userMessage", "itemId": "task", "turnId": "native-turn", "commandId": command, "text": "task"},
                {"kind": "userMessage", "itemId": "steering", "turnId": "native-turn", "commandId": steering, "text": "steer"},
                {"kind": "agentMessage", "itemId": "report", "turnId": "native-turn", "status": "completed", "text": report}]
            rpc = Mock()
            rpc.call.return_value = {"events": [], "nextCursor": None}
            result = {"session": {"sessionId": state.value["sessionId"], "workspaceRoot": "/tasks/7", "activeTurnId": None},
                "history": {"mode": "snapshot", "snapshot": {"state": {"items": items}}},
                "lastTurn": {"turnId": "native-turn", "terminal": "completed"}}
            thread = recovered_thread(rpc, state, result)
            self.assertEqual(thread["turns"][0]["status"], "completed")
            self.assertEqual([i.get("clientId") for i in thread["turns"][0]["items"]], ["task-source", "steering-source", None])
            self.assertEqual(thread["turns"][0]["items"][-1]["phase"], "final_answer")
            self.assertEqual(thread["turns"][0]["items"][-1]["text"], report)
            rpc.call.assert_called_once_with("view/page", {"sessionId": state.value["sessionId"], "limit": 1000})

    def test_paging_confirms_only_native_receipts_and_retains_latest_item_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            state = SessionState(Path(directory) / "state.json", {"name": "vm", "cwd": "/tasks/7"})
            state.command("not-delivered")
            command = state.command("delivered")
            item = {"kind": "userMessage", "itemId": "one", "turnId": "turn", "commandId": command, "text": "old", "revision": 1}
            rpc = Mock()
            rpc.call.side_effect = [
                {"events": [{"method": "item/started", "params": {"item": item}}], "nextCursor": "next"},
                {"events": [{"method": "item/completed", "params": {"item": {**item, "text": "new", "revision": 2}}},
                    {"method": "turn/completed", "params": {"turnId": "turn", "terminal": "cancelled"}}], "nextCursor": None}]
            result = {"session": {"sessionId": state.value["sessionId"], "workspaceRoot": "/tasks/7"}, "history": {"mode": "none"}}
            thread = recovered_thread(rpc, state, result)
            self.assertEqual(thread["turns"], [{"id": "turn", "status": "cancelled", "items": [
                {"type": "userMessage", "id": "one", "clientId": "delivered", "content": [{"type": "text", "text": "new"}]}]}])
            self.assertEqual(rpc.call.call_args.args[1]["cursor"], "next")
            for key, value in [("workspaceRoot", "/tasks/other"), ("sessionId", "other-session")]:
                with self.assertRaisesRegex(RuntimeError, "different session or task"):
                    recovered_thread(rpc, state, {**result, "session": {**result["session"], key: value}})

    def test_recovery_does_not_loop_on_a_stuck_native_cursor(self):
        with tempfile.TemporaryDirectory() as directory:
            state = SessionState(Path(directory) / "state.json", {"name": "vm", "cwd": "/tasks/7"})
            rpc = Mock()
            rpc.call.return_value = {"events": [], "nextCursor": "stuck"}
            with self.assertRaisesRegex(RuntimeError, "cursor did not advance"):
                recovered_thread(rpc, state, {"session": {"sessionId": state.value["sessionId"], "workspaceRoot": "/tasks/7"}})

    def test_missing_native_session_requires_explicit_retry_when_delivery_is_uncertain(self):
        for allow_new in [False, True]:
            with tempfile.TemporaryDirectory() as directory, patch("muse_bridge.ManagedRpc") as client, patch("muse_bridge.subprocess.run"):
                config = {"name": "vm", "cwd": "/tasks/7", "env": {"HOME": "/home/worker"}}
                path = Path(directory) / "state.json"
                state = SessionState(path, config)
                state.command("uncertain")
                state = SessionState(path, config)
                old_session = state.value["sessionId"]
                headers = {"authorization": "Bearer synthetic-provider-secret"}
                missing = muse_transport.RpcError("session/resume", {"code": -32000, "data": {"kind": "sessionNotFound"}})
                client.return_value.call.side_effect = [{"grantedCapabilities": ["sessionMcp"]}, missing, {}]
                if allow_new:
                    rpc, thread = connect(config, [], state, headers, Path(directory), lambda _: None, True)
                    self.assertNotEqual(thread["id"], old_session)
                    self.assertEqual(state.value["commands"], {})
                else:
                    with self.assertRaisesRegex(RuntimeError, "uncertain delivery are retained"):
                        connect(config, [], state, headers, Path(directory), lambda _: None, False)
                    client.return_value.close.assert_called_once()
                    self.assertEqual(client.return_value.call.call_count, 2)
                    self.assertIn("uncertain", state.value["commands"])

    def test_recovery_state_from_another_vm_is_not_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            SessionState(path, {"name": "original-vm", "cwd": "/tasks/7"})
            before = path.read_bytes()
            with self.assertRaisesRegex(RuntimeError, "state is invalid"):
                SessionState(path, {"name": "another-vm", "cwd": "/tasks/7"})
            self.assertEqual(path.read_bytes(), before)

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
