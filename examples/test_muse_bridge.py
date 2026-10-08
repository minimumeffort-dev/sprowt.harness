import sys
from pathlib import Path
import base64
import json
import io
import os
import queue
import tempfile
import threading
import unittest
import urllib.error
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from muse_bridge import ManagedRpc, SessionState, connect, recovered_thread, normalize, final_report, host
from muse_transport import BridgeFailure, Broker, FrameReader, FramedOutput, FRAME_BYTES, provider_failure, failure
import muse_transport


def bridge_fixture():
    bridge = object.__new__(ManagedRpc)
    bridge.broker = Broker({"authorization": "Bearer synthetic-provider-secret"})
    bridge.broker.on_failure = bridge.fail
    bridge.tools = []
    bridge.stopped = False
    bridge.failure = None
    bridge.failure_lock = threading.Lock()
    bridge.executor_lock = threading.Lock()
    bridge.process_id = "fixture-process"
    bridge.executor = Mock()
    bridge.executor.buffered = []
    bridge.incoming = queue.Queue()
    bridge.buffered = []
    bridge.pool = threading.BoundedSemaphore(1)
    return bridge


def rejected(status=400, code="invalid_request_error", message="Invalid request"):
    body = json.dumps({"error": {"code": code, "message": message}}).encode()
    return urllib.error.HTTPError("https://api.meta.ai/v1/responses", status, "rejected", {}, io.BytesIO(body))


def wire_frames(value):
    data = json.dumps(value, ensure_ascii=False).encode()
    return [{"channel": "frame", "seq": index + 1,
             "chunk": base64.b64encode(data[offset:offset + FRAME_BYTES]).decode(),
             "last": offset + FRAME_BYTES >= len(data)}
            for index, offset in enumerate(range(0, len(data), FRAME_BYTES))]


class StreamTests(unittest.TestCase):
    def test_large_message_waits_for_each_ack_and_round_trips(self):
        value = {"channel": "rpc", "value": "π\\\"\n" * 150_000}
        output, reader, wire = FramedOutput(), FrameReader(), queue.Queue()
        errors = []

        def send():
            try:
                output.send(value)
            except Exception as error:
                errors.append(error)

        with patch("builtins.print", side_effect=lambda text, **_: wire.put(text)):
            producer = threading.Thread(target=send, daemon=True)
            producer.start()
            try:
                frame = json.loads(wire.get(timeout=1))
                output.acknowledge(frame["seq"] + 1)
                with self.assertRaises(queue.Empty):
                    wire.get(timeout=0.02)
                while True:
                    self.assertLess(len(json.dumps(frame)), 65536)
                    result = reader.receive(frame)
                    output.acknowledge(frame["seq"])
                    if result is not None:
                        break
                    frame = json.loads(wire.get(timeout=1))
                producer.join(timeout=1)
                self.assertFalse(producer.is_alive())
                self.assertEqual(errors, [])
                self.assertEqual(result, value)
                self.assertGreater(reader.sequence, 32)
            finally:
                output.close()
                producer.join(timeout=1)

    def test_closed_transport_releases_waiting_sender(self):
        output, wire, errors = FramedOutput(), queue.Queue(), []

        def send():
            try:
                output.send({"fixture": "payload"})
            except BridgeFailure as error:
                errors.append(str(error))

        with patch("builtins.print", side_effect=lambda text, **_: wire.put(text)):
            producer = threading.Thread(target=send, daemon=True)
            producer.start()
            wire.get(timeout=1)
            output.close()
            producer.join(timeout=1)
            self.assertFalse(producer.is_alive())
            self.assertEqual(errors, ["Muse output transport closed."])

    def test_invalid_missing_and_oversized_frames_do_not_expose_payloads(self):
        frame = wire_frames({"private": "synthetic-private-value"})[0]
        for change in ({"seq": 2}, {"seq": True}, {"chunk": "private-not-base64"},
                       {"last": "yes"}, {"chunk": base64.b64encode(b"private-not-json").decode()}):
            with self.assertRaises(BridgeFailure) as caught:
                FrameReader().receive({**frame, **change})
            self.assertNotIn("private", str(caught.exception))
        reader = FrameReader()
        reader.receive(frame)
        with self.assertRaises(BridgeFailure):
            reader.receive(frame)
        with patch("muse_transport.MESSAGE_BYTES", 10):
            with self.assertRaises(BridgeFailure):
                FrameReader().receive(frame)
            with self.assertRaises(BridgeFailure):
                FramedOutput().send({"private": "synthetic-private-value"})

    def test_closed_process_is_drained_across_pages_before_exit(self):
        bridge = bridge_fixture()
        bridge.executor.buffered.append({"method": "process/output"})
        value = {"method": "item/completed", "params": {"text": "λ" * 80_000}}
        frames = wire_frames({"channel": "rpc", "value": value})
        wire = b"".join(json.dumps(frame).encode() + b"\n" for frame in frames)
        pages = iter([{"chunks": [{"stream": "stdout", "chunk": base64.b64encode(wire[i:i + 10001]).decode()}],
                       "closed": True, "nextSeq": n + 2}
                      for n, i in enumerate(range(0, len(wire), 10001))] + [{"chunks": [], "closed": True}])
        acks = []

        def call(method, params):
            if method == "process/read":
                return next(pages)
            acks.append(json.loads(base64.b64decode(params["chunk"])))
            return {"status": "accepted"}

        bridge.executor.call.side_effect = call
        bridge.read()
        self.assertEqual(bridge.incoming.get_nowait(), value)
        self.assertEqual([ack["seq"] for ack in acks], list(range(1, len(frames) + 1)))
        self.assertEqual(bridge.executor.buffered, [])
        self.assertIn("stream closed", str(bridge.failure))


class FailureTests(unittest.TestCase):
    def test_provider_rejection_stops_retries_and_keeps_redacted_cause(self):
        bridge = bridge_fixture()
        secret, cap = bridge.broker.secret.decode(), bridge.broker.capability
        message = f"Invalid context: {secret} {base64.b64encode(secret.encode()).decode()} {cap} https://private.invalid/?token=secret\n\x1bsecret"
        bridge.broker.opener = Mock()
        bridge.broker.opener.open.side_effect = rejected(message=message)
        request = {"method": "POST", "path": "/responses", "authorization": "Bearer " + cap,
                   "body": base64.b64encode(json.dumps({"model": muse_transport.MODEL, "stream": True}).encode()).decode()}
        replies = []
        bridge.broker.forward(request, replies.append)
        bridge.broker.forward(request, replies.append)
        bridge.broker.opener.open.assert_called_once()
        with self.assertRaisesRegex(BridgeFailure, "HTTP 400") as caught:
            bridge.next()
        diagnostic = str(caught.exception)
        self.assertIn("invalid_request_error", diagnostic)
        self.assertNotIn(secret, diagnostic)
        self.assertNotIn(cap, diagnostic)
        self.assertNotIn("private.invalid", diagnostic)
        self.assertTrue(all(c.isprintable() for c in diagnostic))
        self.assertNotIn(secret, json.dumps(replies))
        bridge.fail(BridgeFailure("later shutdown failure"))
        self.assertIs(bridge.failure, caught.exception)

    def test_transient_http_failures_keep_native_retries(self):
        for status in (408, 429, 500, 502, 503):
            bridge = bridge_fixture()
            bridge.broker.opener = Mock()
            bridge.broker.opener.open.side_effect = rejected(status)
            request = {"method": "GET", "path": "/muse-code/models", "body": "",
                       "authorization": "Bearer " + bridge.broker.capability}
            replies = []
            bridge.broker.forward(request, replies.append)
            bridge.broker.forward(request, replies.append)
            self.assertEqual(bridge.broker.opener.open.call_count, 2)
            self.assertIsNone(bridge.failure)
            self.assertEqual(replies[-1]["status"], status)

    def test_oversized_malformed_or_non_json_errors_stay_bounded(self):
        for body in (b"secret " * 1500, b"not json", b"[]", b'{"error":"private text"}'):
            error = urllib.error.HTTPError("unused", 400, "rejected", {}, io.BytesIO(body))
            diagnostic = provider_failure(error, [])
            self.assertEqual(diagnostic.details["recovery"], "retry")
            self.assertLess(len(str(diagnostic)), 160)
            self.assertNotIn("private text", str(diagnostic))

    def test_transport_diagnostics_do_not_dump_exception_content(self):
        for error in (ValueError("private body"), OSError(32, "private body"), queue.Empty()):
            self.assertNotIn("private body", str(failure(error, "VM transport")))
        self.assertIn("timed out", str(failure(queue.Empty(), "VM transport")))

    def test_error_details_redact_quoted_credentials_and_auth_failures_do_not_reset(self):
        diagnostic = provider_failure(rejected(401, "context_length_exceeded",
            'Invalid "api_key": "private-value", password=also-private'), [])
        self.assertNotIn("private-value", str(diagnostic))
        self.assertNotIn("also-private", str(diagnostic))
        self.assertIn("muse login", str(diagnostic))
        self.assertEqual(diagnostic.details["recovery"], "retry")

    def test_guest_exit_and_broken_transport_keep_their_cause(self):
        bridge = bridge_fixture()
        frame = json.dumps(wire_frames({"channel": "ended", "exit_code": -9})[0]).encode() + b"\n"
        bridge.executor.call.side_effect = [{"chunks": [{"stream": "stdout", "chunk": base64.b64encode(frame).decode()}]}, {"status": "accepted"}]
        bridge.read()
        with self.assertRaisesRegex(BridgeFailure, "code -9"):
            bridge.next()
        self.assertEqual(bridge.failure.details["exit_code"], -9)
        for problem in (queue.Empty(), OSError(32, "private"), {"bad": "shape"}):
            bridge = bridge_fixture()
            bridge.executor.call.side_effect = [problem]
            bridge.read()
            with self.assertRaisesRegex(BridgeFailure, "VM transport"):
                bridge.next()

    def test_shutdown_still_terminates_after_a_broken_input_pipe(self):
        bridge = bridge_fixture()
        original = BridgeFailure("Muse provider HTTP 400.")
        bridge.fail(original)
        bridge.executor.call.side_effect = [BrokenPipeError(), {}]
        bridge.close()
        self.assertEqual([c.args[0] for c in bridge.executor.call.call_args_list], ["process/write", "process/terminate"])
        bridge.executor.close.assert_called_once()
        self.assertIs(bridge.failure, original)

    def test_context_recovery_retains_receipts_and_requires_safe_retry(self):
        for allow_new, accepted in ((False, True), (True, False), (True, True)):
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "state.json"
                config = {"name": "vm", "cwd": "/tasks/7"}
                state = SessionState(path, config)
                source = state.command("source")
                if accepted:
                    state.accepted("source", {"status": "accepted", "turnId": "turn"})
                state.record_failure(provider_failure(rejected(code="context_length_exceeded"), []))
                before = path.read_bytes()
                reloaded = SessionState(path, config)
                if allow_new and accepted:
                    reloaded.recover(True)
                    self.assertFalse(reloaded.resuming)
                    self.assertNotEqual(reloaded.value["sessionId"], state.value["sessionId"])
                    self.assertEqual(reloaded.value["previousSession"]["commands"]["source"]["id"], source)
                    self.assertEqual(reloaded.value["commands"], {})
                    self.assertNotIn("failure", reloaded.value)
                    again = SessionState(path, config)
                    again.recover(True)
                    self.assertEqual(again.value["sessionId"], reloaded.value["sessionId"])
                else:
                    reloaded.recover(allow_new)
                    self.assertTrue(reloaded.resuming)
                    self.assertEqual(path.read_bytes(), before)

    def test_unclassified_400_does_not_reset_the_conversation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            config = {"name": "vm", "cwd": "/tasks/7"}
            state = SessionState(path, config)
            state.command("uncertain")
            state.record_failure(provider_failure(rejected(), []))
            before = path.read_bytes()
            SessionState(path, config).recover(True)
            self.assertEqual(path.read_bytes(), before)

    def test_resumed_review_failure_saves_diagnostics_and_accepted_delivery(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            config = {"name": "vm", "cwd": "/tasks/7"}
            saved = SessionState(path, config)
            saved.command("first-task")
            saved.accepted("first-task", {"status": "accepted", "turnId": "first-turn"})
            bridge = bridge_fixture()
            bridge.write = Mock()
            bridge.call = Mock(return_value={"status": "accepted", "turnId": "repair-turn"})
            bridge.close = Mock()
            bridge.fail(provider_failure(rejected(message="Invalid request payload"), []))
            request = {"id": 1, "method": "turn/start", "params": {"state": str(path), "config": config,
                       "tools": [], "text": "Fix the review finding", "source": "review-fix", "schema": {}}}
            output = io.StringIO()
            with patch("muse_bridge.host_login", return_value={}), patch("muse_bridge.connect", return_value=(bridge, {"id": saved.value["sessionId"]})), \
                    patch("sys.stdin", io.StringIO(json.dumps(request) + "\n")), patch("sys.stdout", output):
                with self.assertRaisesRegex(BridgeFailure, "HTTP 400"):
                    host()
            reloaded = SessionState(path, config)
            self.assertEqual(reloaded.value["commands"]["review-fix"]["turnId"], "repair-turn")
            self.assertEqual(reloaded.value["commands"]["first-task"]["turnId"], "first-turn")
            self.assertEqual(reloaded.value["failure"]["http_status"], 400)
            self.assertIn('"result": {"status": "accepted"', output.getvalue())
            bridge.close.assert_called_once()

    def test_unexpected_request_failure_is_safe_and_keeps_delivery_state(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            config = {"name": "vm", "cwd": "/tasks/7"}
            bridge = bridge_fixture()
            bridge.close = Mock()
            bridge.call = Mock(side_effect=TypeError("Bearer synthetic-secret"))
            request = {"id": 1, "method": "turn/start", "params": {"state": str(path), "config": config,
                       "tools": [], "text": "Task", "source": "task", "schema": {}}}
            output = io.StringIO()
            with patch("muse_bridge.host_login", return_value={}), patch("muse_bridge.connect", return_value=(bridge, {"id": "session"})), \
                    patch("sys.stdin", io.StringIO(json.dumps(request) + "\n")), patch("sys.stdout", output):
                host()
            saved = json.loads(path.read_text())
            self.assertIn("task", saved["commands"])
            self.assertNotIn("turnId", saved["commands"]["task"])
            self.assertIn("TypeError", saved["failure"]["message"])
            self.assertNotIn("synthetic-secret", output.getvalue() + path.read_text())
            reply = [json.loads(line) for line in output.getvalue().splitlines() if '"error"' in line][0]
            self.assertEqual(reply["error"]["data"]["failure"], saved["failure"])
            self.assertEqual(reply["error"]["data"]["delivery"], "unknown")
            bridge.close.assert_called_once()

    def test_completed_turn_followed_by_exit_keeps_the_completed_report(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            config = {"name": "vm", "cwd": "/tasks/7"}
            bridge = bridge_fixture()
            bridge.call = Mock(return_value={"status": "accepted", "turnId": "turn"})
            bridge.close = Mock()
            report = '{"status":"completed","summary":"Done","checks":[]}'
            bridge.incoming.put({"method": "item/completed", "params": {"item": {
                "kind": "agentMessage", "itemId": "final", "text": report}}})
            bridge.incoming.put({"method": "turn/completed", "params": {"turnId": "turn", "terminal": "completed"}})
            bridge.fail(BridgeFailure("guest exited"))
            request = {"id": 1, "method": "turn/start", "params": {"state": str(path), "config": config,
                       "tools": [], "text": "Task", "source": "task", "schema": {}}}
            output = io.StringIO()
            with patch("muse_bridge.host_login", return_value={}), patch("muse_bridge.connect", return_value=(bridge, {"id": "session"})), \
                    patch("sys.stdin", io.StringIO(json.dumps(request) + "\n")), patch("sys.stdout", output):
                host()
            events = [json.loads(line) for line in output.getvalue().splitlines()]
            self.assertEqual(events[-2]["params"]["item"]["text"], report)
            self.assertEqual(events[-1]["params"]["turn"]["status"], "completed")
            self.assertNotIn("failure", json.loads(path.read_text()))
            bridge.close.assert_called_once()


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
            saved = SessionState(state.path, {"name": "vm", "cwd": "/tasks/7"})
            self.assertEqual(saved.value["commands"]["task-source"]["turnId"], "native-turn")
            self.assertEqual(saved.value["commands"]["steering-source"]["turnId"], "native-turn")
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
            self.assertEqual(thread["dispatchJournal"], ["not-delivered", "delivered"])
            self.assertEqual(rpc.call.call_args.args[1]["cursor"], "next")
            for key, value in [("workspaceRoot", "/tasks/other"), ("sessionId", "other-session")]:
                with self.assertRaisesRegex(RuntimeError, "different session or task"):
                    recovered_thread(rpc, state, {**result, "session": {**result["session"], key: value}})

    def test_dispatch_journal_distinguishes_unsent_from_uncertain_after_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {"name": "vm", "cwd": "/tasks/7"}
            state = SessionState(Path(directory) / "state.json", config)
            state.command("uncertain")
            state.command("accepted")
            state.accepted("accepted", {"status": "accepted", "turnId": "turn"})
            state = SessionState(state.path, config)
            rpc = Mock()
            rpc.call.return_value = {"events": [], "nextCursor": None}
            thread = recovered_thread(rpc, state, {"session": {
                "sessionId": state.value["sessionId"], "workspaceRoot": "/tasks/7"}})
            self.assertEqual(thread["turns"], [])
            self.assertEqual(thread["dispatchJournal"], ["uncertain", "accepted"])
            self.assertNotIn("never-sent", thread["dispatchJournal"])
            rpc.call.assert_called_once_with("view/page", {"sessionId": state.value["sessionId"], "limit": 1000})

    def test_missing_terminal_is_unknown_not_an_interrupted_turn(self):
        with tempfile.TemporaryDirectory() as directory:
            state = SessionState(Path(directory) / "state.json", {"name": "vm", "cwd": "/tasks/7"})
            command = state.command("task")
            rpc = Mock()
            rpc.call.return_value = {"events": [], "nextCursor": None}
            result = {"session": {"sessionId": state.value["sessionId"], "workspaceRoot": "/tasks/7"},
                      "history": {"items": [{"kind": "userMessage", "itemId": "one", "turnId": "turn", "commandId": command}]}}
            self.assertEqual(recovered_thread(rpc, state, result)["turns"][0]["status"], "unknown")
            result["session"]["activeTurnId"] = "turn"
            self.assertEqual(recovered_thread(rpc, state, result)["turns"][0]["status"], "inProgress")
            result["session"].pop("activeTurnId")
            result["lastTurn"] = {"turnId": "turn", "terminal": "interrupted"}
            self.assertEqual(recovered_thread(rpc, state, result)["turns"][0]["status"], "interrupted")

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
