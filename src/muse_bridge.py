"""Muse RPC adapter. Provider credentials exist only in the host process."""

import base64
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

from muse_transport import (BridgeFailure, Broker, CHECKSUM, MODEL, VERSION, FrameReader, Rpc, RpcError,
                            command_id, failure, guest, host_login, private_json, read_account, settings)


def artifact(cache):
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = cache / ("muse-" + VERSION)

    def digest(file):
        result = hashlib.sha256()
        with file.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                result.update(chunk)
        return result.hexdigest()

    if path.exists() and digest(path) == CHECKSUM:
        return path
    info = json.loads((Path.home() / ".local/bin/.muse-release-info.json").read_text())
    if info.get("version") != VERSION or not info.get("manifest_url", "").startswith("https://lookaside.facebook.com/"):
        raise BridgeFailure("Install the supported Muse account CLI: " + VERSION)
    with urllib.request.urlopen(info["manifest_url"], timeout=30) as response:
        manifest = json.load(response)
    item = manifest["artifacts"]["aarch64_linux"]
    if (manifest.get("version") != VERSION or item.get("checksum") != CHECKSUM
            or not item.get("url", "").startswith("https://lookaside.facebook.com/")):
        raise BridgeFailure("Muse release did not match the pinned Linux binary.")
    next_path = path.with_suffix(".next-" + secrets.token_hex(8))
    try:
        with urllib.request.urlopen(item["url"], timeout=60) as source, next_path.open("xb") as output:
            next_path.chmod(0o600)
            while chunk := source.read(1024 * 1024):
                output.write(chunk)
        if digest(next_path) != CHECKSUM or next_path.stat().st_size != item["size"]:
            raise BridgeFailure("Muse binary checksum mismatch.")
        next_path.chmod(0o755)
        next_path.replace(path)
    finally:
        next_path.unlink(missing_ok=True)
    return path


def cleanup_children(pid):
    parents = {}
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                parents[int(entry.name)] = int((entry / "stat").read_text().rsplit(")", 1)[1].split()[1])
            except (OSError, ValueError, IndexError):
                pass
    descendants = {pid}
    for _ in range(len(parents)):
        found = {child for child, parent in parents.items() if parent in descendants}
        if found <= descendants:
            break
        descendants.update(found)
    for child in sorted(descendants, reverse=True):
        try:
            os.kill(child, signal.SIGKILL)
        except ProcessLookupError:
            pass


def guest_client(base):
    home = Path.home()
    if Path("/workspace").exists() or Path("/opt/sprowt-git").exists():
        raise BridgeFailure("Muse filesystem boundary includes controller source.")
    value = settings(base)
    value["run"].pop("toolset")
    private_json(home / ".config/muse/settings.json", value)
    private_json(home / ".sprowt-endpoint.json", {"url": base})
    client = subprocess.Popen(["/opt/sprowt-muse/muse", "serve", "--disable-sandbox"],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True)
    # The enclosing exec-server boundary applies to Muse and every child tool.
    original_kill = client.kill

    def kill():
        cleanup_children(client.pid)
        original_kill()

    client.kill = kill
    return client


def mcp():
    home = Path.home()
    endpoint = json.loads((home / ".sprowt-endpoint.json").read_text())["url"]
    cap = json.loads((home / ".config/muse/auth.json").read_text())["providers"]["meta"]["api_key"]
    tools = json.loads((home / ".sprowt-tools.json").read_text())
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message.get("method")
        if method == "initialize":
            result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                      "serverInfo": {"name": "sprowt", "version": "0.1"}}
        elif method == "tools/list":
            result = {"tools": [{key: tool[key] for key in ("name", "description", "inputSchema")} for tool in tools]}
        elif method == "tools/call":
            request = urllib.request.Request(endpoint + "/sprowt/tools",
                data=json.dumps(message["params"]).encode(),
                headers={"Authorization": "Bearer " + cap, "Content-Type": "application/json"})
            try:
                with urllib.request.urlopen(request, timeout=960) as response:
                    result = json.load(response)
            except OSError:
                result = {"isError": True, "content": [{"type": "text", "text": "Harness tool unavailable."}]}
        elif method == "ping":
            result = {}
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "Unsupported MCP method"}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)


class ManagedRpc(Rpc):
    def __init__(self, config, broker, emit, tools):
        self.broker, self.emit, self.tools = broker, emit, tools
        self.writing = threading.Lock()
        self.executor = Rpc(config["executor"])
        self.executor_lock = threading.Lock()
        self.stopped = False
        self.process_id = "sprowt-muse-" + secrets.token_hex(8)
        self.counter, self.buffered, self.incoming = 0, [], queue.Queue()
        self.failure = None
        self.failure_lock = threading.Lock()
        self.broker.on_failure = self.fail
        self.pool = threading.BoundedSemaphore(4)
        try:
            self.start(config)
        except (OSError, RuntimeError, ValueError, queue.Empty):
            self.broker.revoked = True
            self.executor.close()
            raise
        threading.Thread(target=self.read, daemon=True).start()

    def fail(self, diagnostic):
        with self.failure_lock:
            if self.failure is None and not self.stopped:
                self.failure = diagnostic
                self.broker.revoked = True
                self.incoming.put(None)

    def start(self, config):
        self.executor.call("initialize", {"clientName": "sprowt_muse"})
        self.executor.write({"method": "initialized"})
        result = self.executor.call("process/start", {"processId": self.process_id,
            "argv": ["/usr/bin/python3", "-u", "/opt/sprowt-muse/bridge.py", "--guest"],
            "cwd": "file://" + config["cwd"], "env": config["env"],
            "envPolicy": {"inherit": "none", "ignoreDefaultExcludes": True, "exclude": [], "set": {}, "includeOnly": []},
            "tty": False, "pipeStdin": True, "arg0": None, "sandbox": config["sandbox"],
            "enforceManagedNetwork": True, "networkProxy": config["proxy"]})
        if result.get("sandboxType") != "linuxSeccomp":
            self.close()
            raise BridgeFailure("Muse requires an enforced guest process sandbox.")

    def envelope(self, value):
        with self.executor_lock:
            result = self.executor.call("process/write", {"processId": self.process_id,
                "writeId": secrets.token_hex(16),
                "chunk": base64.b64encode((json.dumps(value) + "\n").encode()).decode()})
            if result.get("status") != "accepted":
                raise BridgeFailure("Guest stdin is unavailable.")

    def write(self, value):
        self.envelope({"channel": "rpc", "value": {"jsonrpc": "2.0", **value}})

    def relay(self, value):
        try:
            if value["path"] == "/sprowt/tools":
                if (not self.broker.revoked and self.broker.allow({**value, "method": "GET", "path": "/muse-code/models", "body": ""})
                        and value["method"] == "POST"):
                    params = json.loads(base64.b64decode(value["body"], validate=True))
                    if (not isinstance(params, dict) or not isinstance(params.get("name"), str)
                            or not isinstance(params.get("arguments", {}), dict)
                            or params.get("name") not in {tool["name"] for tool in self.tools}):
                        raise BridgeFailure("Tool not advertised")
                    self.emit({"method": "item/tool/call", "id": "tool:" + value["id"], "params": params})
                    return
                self.envelope({"channel": "http", "id": value["id"], "status": 403, "done": True})
            else:
                self.broker.forward(value, lambda reply: self.envelope({"channel": "http", "id": value["id"], **reply}))
        except (OSError, RuntimeError, ValueError, queue.Empty):
            if not self.stopped:
                try:
                    self.envelope({"channel": "http", "id": value["id"], "status": 502, "done": True})
                except (OSError, RuntimeError, ValueError, queue.Empty) as error:
                    self.fail(failure(error, "guest input"))
        finally:
            self.pool.release()

    def read(self):
        cursor, pending = 0, b""
        frames = FrameReader()
        try:
            while not self.stopped:
                with self.executor_lock:
                    read = self.executor.call("process/read", {"processId": self.process_id, "afterSeq": cursor, "maxBytes": 65536, "waitMs": 20})
                    self.executor.buffered.clear()
                for chunk in read["chunks"]:
                    if chunk["stream"] != "stdout":
                        continue
                    pending += base64.b64decode(chunk["chunk"])
                    if len(pending) > 4 * 1024 * 1024:
                        raise BridgeFailure("Muse guest output exceeded the 4 MiB frame limit.", stage="guest output")
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        try:
                            frame = json.loads(line)
                        except ValueError:
                            raise BridgeFailure("Muse VM output was truncated or malformed.", stage="VM transport") from None
                        if not isinstance(frame, dict):
                            raise BridgeFailure("Muse VM output frame is invalid.", stage="VM transport")
                        value = frames.receive(frame)
                        self.envelope({"channel": "ack", "seq": frame["seq"]})
                        if value is None:
                            continue
                        if value.get("channel") == "http":
                            if not self.pool.acquire(blocking=False):
                                self.envelope({"channel": "http", "id": value["id"], "status": 429, "done": True})
                            else:
                                threading.Thread(target=self.relay, args=(value,), daemon=True).start()
                        elif value.get("channel") == "rpc":
                            self.incoming.put(value["value"])
                        elif value.get("channel") == "ended":
                            code = value.get("exit_code")
                            code = code if type(code) is int else None
                            self.fail(BridgeFailure(f"Muse guest exited (code {code}).", stage="guest exit", exit_code=code))
                            return
                        elif value.get("channel") == "failed":
                            self.fail(BridgeFailure("Muse CLI returned invalid output inside the VM.", stage="guest output"))
                            return
                        else:
                            self.fail(BridgeFailure("Muse guest sent invalid protocol output.", stage="guest output"))
                            return
                cursor = max(cursor, read.get("nextSeq", 1) - 1)
                if read.get("closed") and not read["chunks"]:
                    self.fail(BridgeFailure("Muse VM process stream closed before the turn finished.", stage="VM transport"))
                    return
                time.sleep(0.01)
        except (OSError, RuntimeError, ValueError, KeyError, TypeError, queue.Empty) as error:
            self.fail(failure(error, "VM transport"))
        finally:
            self.incoming.put(None)

    def next(self, timeout=45):
        if self.failure:
            raise self.failure
        value = self.incoming.get(timeout=timeout)
        if value is None:
            raise self.failure or BridgeFailure("Muse guest ended before responding.", stage="guest exit")
        return value

    def close(self):
        self.broker.revoked = True
        self.stopped = True
        try:
            # Let the wrapper clean up native tools before terminating its boundary.
            try:
                with self.executor_lock:
                    self.executor.call("process/write", {"processId": self.process_id, "writeId": secrets.token_hex(16), "chunk": base64.b64encode(b'{"channel":"stop"}\n').decode()})
                time.sleep(0.1)
            finally:
                with self.executor_lock:
                    self.executor.call("process/terminate", {"processId": self.process_id})
        except (OSError, RuntimeError, ValueError, queue.Empty):
            # A dead guest cannot acknowledge shutdown; retain the original failure.
            pass
        finally:
            self.executor.close()


def final_report(text):
    try:
        report = json.loads(text)
        return isinstance(report, dict) and "status" in report and "checks" in report and "summary" in report
    except ValueError:
        return False


def normalize(message):
    method, params = message.get("method"), message.get("params", {})
    if method == "item/completed" and params.get("item", {}).get("kind") == "agentMessage":
        item = params["item"]
        return {"method": method, "params": {"item": {"type": "agentMessage", "id": item.get("itemId", item.get("id")),
            "text": item.get("text", ""), "phase": "final_answer" if item.get("phase") in {"final", "final_answer"} or final_report(item.get("text", "")) else "commentary"}}}
    if method == "turn/completed":
        return {"method": method, "params": {"turn": {"id": params["turnId"], "status": params["terminal"]}}}
    return None


class SessionState:
    def __init__(self, path, config):
        self.path = Path(path)
        saved = json.loads(self.path.read_text()) if self.path.exists() else {}
        if saved and (not isinstance(saved, dict) or saved.get("vm") != config["name"]
                or type(saved.get("task")) is not int or saved["task"] <= 0
                or saved.get("cwd") != f'/tasks/{saved["task"]}'
                or not isinstance(saved.get("commands"), dict)
                or not all(isinstance(command, dict) and isinstance(command.get("id"), str)
                           for command in saved["commands"].values())
                or not isinstance(saved.get("sessionId"), str) or not isinstance(saved.get("startId"), str)):
            raise BridgeFailure("Saved Muse recovery state is invalid; work is retained.")
        self.resuming = bool(saved) and saved.get("cwd") == config["cwd"]
        self.value = saved if self.resuming else {
            "vm": config["name"], "cwd": config["cwd"], "task": int(config["cwd"].rsplit("/", 1)[1]),
            "sessionId": command_id(), "startId": command_id(), "commands": {}}
        self.save()

    def save(self):
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        next_path = self.path.with_suffix(".next")
        with next_path.open("w") as output:
            os.chmod(next_path, 0o600)
            json.dump(self.value, output)
            output.flush()
            os.fsync(output.fileno())
        next_path.replace(self.path)
        directory = os.open(self.path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)

    def record_failure(self, diagnostic):
        self.value["failure"] = diagnostic.details
        self.save()

    def recover(self, allow_new):
        diagnostic = self.value.get("failure") or {}
        if diagnostic.get("recovery") != "fresh_session":
            return
        if not allow_new or any(not c.get("turnId") for c in self.value["commands"].values()):
            return
        self.value["previousSession"] = {"id": self.value["sessionId"], "commands": self.value["commands"]}
        self.value.update(sessionId=command_id(), startId=command_id(), commands={})
        self.value.pop("failure", None)
        self.resuming = False
        self.save()

    def command(self, source):
        commands = self.value["commands"]
        if source not in commands:
            commands[source] = {"id": command_id()}
            self.save()
        return commands[source]["id"]

    def accepted(self, source, result):
        if result.get("status") != "accepted" or not isinstance(result.get("turnId"), str):
            raise BridgeFailure("Muse returned no accepted turn ID.")
        self.value["commands"][source]["turnId"] = result["turnId"]
        self.save()


def recovered_thread(rpc, state, result):
    session = result["session"]
    if session["sessionId"] != state.value["sessionId"] or session["workspaceRoot"] != state.value["cwd"]:
        raise BridgeFailure("Muse recovery returned a different session or task folder.")
    sources = {v["id"]: source for source, v in state.value["commands"].items()}
    turns, items, cursor, size = {}, {}, None, 0

    def fold(item):
        if item.get("kind") not in {"userMessage", "agentMessage"} or not item.get("turnId"):
            return
        key = item["itemId"]
        if key not in items or item.get("revision", 0) >= items[key].get("revision", 0):
            items[key] = item

    # Native inline history can be downgraded. Page durable events for receipts and terminals.
    for _ in range(64):
        params = {"sessionId": session["sessionId"], "limit": 1000}
        if cursor:
            params["cursor"] = cursor
        page = rpc.call("view/page", params)
        size += len(json.dumps(page))
        if size > 8 * 1024 * 1024:
            raise BridgeFailure("Muse recovery history exceeds its limit; delivery remains unconfirmed.")
        for event in page["events"]:
            params = event["params"]
            if event["method"].startswith("item/") and "item" in params:
                fold(params["item"])
            elif event["method"] == "turn/completed":
                turns[params["turnId"]] = params["terminal"]
        next_cursor = page["nextCursor"]
        if next_cursor is None:
            break
        if next_cursor == cursor:
            raise BridgeFailure("Muse recovery cursor did not advance.")
        cursor = next_cursor
    else:
        raise BridgeFailure("Muse recovery history exceeds its limit; delivery remains unconfirmed.")
    history = result.get("history", {})
    snapshot = history.get("snapshot") or {}
    for item in history.get("items") or (snapshot.get("state") or {}).get("items") or []:
        fold(item)
    last = result.get("lastTurn")
    if last:
        turns[last["turnId"]] = last["terminal"]
    if session.get("activeTurnId"):
        turns[session["activeTurnId"]] = "inProgress"
    grouped = {}
    confirmed = False
    for item in items.values():
        if item["kind"] == "userMessage":
            source = sources.get(item.get("commandId"))
            if not source:
                continue
            if not state.value["commands"][source].get("turnId"):
                state.value["commands"][source]["turnId"] = item["turnId"]
                confirmed = True
            value = {"type": "userMessage", "id": item["itemId"], "clientId": source,
                     "content": [{"type": "text", "text": item.get("text", "")} ]}
        elif item.get("status") == "completed":
            value = normalize({"method": "item/completed", "params": {"item": item}})["params"]["item"]
        else:
            continue
        grouped.setdefault(item["turnId"], []).append(value)
    if confirmed:
        state.save()
    return {"id": session["sessionId"], "turns": [
        {"id": turn, "status": turns.get(turn, "unknown"), "items": values}
        for turn, values in grouped.items()]}


def connect(config, tools, state, headers, root, emit, allow_new):
    state.recover(allow_new)
    broker = Broker(headers, requests=128, lifetime=3600, output_tokens=8192)
    rpc = None
    try:
        guest_files = root / "guest"
        guest_files.mkdir(exist_ok=True)
        auth = {"schema_version": 1, "providers": {"meta": {"api_key": broker.capability}}}
        for name, data in [("auth.json", auth), (".sprowt-tools.json", tools)]:
            private_json(guest_files / name, data)
            destination = config["env"]["HOME"] + ("/.config/muse/auth.json" if name == "auth.json" else "/" + name)
            subprocess.run(["container", "copy", str(guest_files / name), config["name"] + ":" + destination],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        rpc = ManagedRpc(config, broker, emit, tools)
        init = rpc.call("initialize", {"clientInfo": {"name": "sprowt_harness", "version": "0.1"},
            "capabilities": {"experimentalApi": True, "requestedCapabilities": ["sessionMcp"], "userInputDialogs": False}})
        if "sessionMcp" not in init.get("grantedCapabilities", []):
            raise BridgeFailure("Muse did not enable session MCP tools.")
        rpc.write({"method": "initialized"})
        mcp_config = {"mcpServers": {"sprowt": {"transport": "stdio", "command": "/usr/bin/python3",
            "args": ["/opt/sprowt-muse/bridge.py", "--mcp"], "framing": "lineDelimitedJson", "mode": "required"}}}
        if state.resuming:
            try:
                result = rpc.call("session/resume", {"commandId": command_id(), "sessionId": state.value["sessionId"],
                    "history": "inline", "config": mcp_config})
                thread = recovered_thread(rpc, state, result)
                # Resume notifications are already represented by the folded history.
                rpc.buffered.clear()
                return rpc, thread
            except RpcError as error:
                if error.kind != "sessionNotFound":
                    raise
                if not allow_new:
                    raise BridgeFailure("Muse session is missing. Work and uncertain delivery are retained; Ctrl+R retries recovery.") from error
                # A removed VM has no native log. An explicit retry may rebuild from saved source.
                state.value.update(sessionId=command_id(), startId=command_id(), commands={})
                state.save()
        rpc.call("session/start", {"commandId": state.value["startId"], "sessionId": state.value["sessionId"],
            "workspaceRoot": config["cwd"], "providerId": "meta", "modelId": MODEL,
            "approvalMode": "allowAll", "config": mcp_config})
        state.resuming = True
        return rpc, {"id": state.value["sessionId"], "turns": []}
    except Exception:
        broker.revoked = True
        if rpc:
            rpc.close()
        raise


def host():
    lock, requests = threading.Lock(), queue.Queue()

    def emit(value):
        with lock:
            print(json.dumps(value), flush=True)

    def receive():
        try:
            for line in sys.stdin:
                requests.put(json.loads(line))
        finally:
            requests.put(None)

    threading.Thread(target=receive, daemon=True).start()
    rpc, state, root = None, None, tempfile.TemporaryDirectory(prefix="sprowt-muse-auth-")
    stage = "account login"
    try:
        headers = host_login(Path(root.name), quiet=True)
        emit({"method": "bridge/ready"})
        while True:
            try:
                message = requests.get(timeout=0.025)
                if message is None:
                    break
            except queue.Empty:
                message = None
            if message and str(message.get("id", "")).startswith("tool:"):
                if rpc:
                    result = message.get("result", {"isError": True, "content": [{"type": "text", "text": "Tool rejected."}]})
                    rpc.envelope({"channel": "http", "id": message["id"][5:], "status": 200,
                        "chunk": base64.b64encode(json.dumps(result).encode()).decode(), "done": True})
                continue
            if message:
                method, params = message["method"], message.get("params", {})
                stage = method
                try:
                    if method in {"session/resume", "turn/start"}:
                        if rpc:
                            rpc.close()
                        config = params["config"]
                        state = SessionState(params["state"], config)
                        rpc, thread = connect(config, params["tools"], state, headers, Path(root.name), emit,
                                              params.get("allowNew", False))
                        session = thread["id"]
                        if method == "session/resume":
                            result = {"thread": thread}
                        else:
                            if (state.value.get("failure") or {}).get("recovery") == "fresh_session":
                                raise BridgeFailure("Muse context is full. Delivery receipts are retained; Ctrl+R retries recovery.",
                                                    stage="session recovery", recovery="fresh_session")
                            emit({"method": "thread/started", "params": {"thread": {"id": session}}})
                            text = params["text"] + "\nReturn only a JSON object as your final answer matching this schema: " + json.dumps(params["schema"])
                            result = rpc.call("turn/start", {"commandId": state.command(params["source"]), "sessionId": session,
                                "reasoningEffort": "high", "input": [{"type": "text", "text": text}]})
                            state.accepted(params["source"], result)
                    elif method == "turn/steer":
                        result = rpc.call(method, {"commandId": state.command(params["source"]), "sessionId": session,
                            "expectedTurnId": params["turn"], "input": [{"type": "text", "text": text} for text in params["texts"]]})
                        state.accepted(params["source"], result)
                    elif method == "turn/interrupt":
                        rpc.call(method, {"commandId": command_id(), "sessionId": session, "turnId": params["turn"]})
                        result = {}
                    elif method == "shutdown":
                        if rpc:
                            rpc.close()
                            rpc = None
                        emit({"id": message["id"], "result": {}})
                        break
                    else:
                        raise BridgeFailure("Unsupported bridge method.")
                    emit({"id": message["id"], "result": result})
                except Exception as error:
                    diagnostic = failure(error, stage)
                    if state:
                        state.record_failure(diagnostic)
                    rejected = (isinstance(error, RpcError) and error.method == method
                        and method in {"turn/start", "turn/steer"}
                        and error.kind in {"commandRejected", "invalidParams", "sessionNotFound"})
                    emit({"id": message["id"], "error": {"code": -32000, "message": str(diagnostic),
                        "data": {"delivery": "rejected" if rejected else "unknown", "failure": diagnostic.details}}})
                    if method in {"turn/start", "session/resume"} and rpc:
                        rpc.close()
                        rpc = None
            if rpc:
                stage = "turn"
                messages, rpc.buffered = rpc.buffered, []
                ended = False
                while not rpc.incoming.empty():
                    value = rpc.incoming.get()
                    if value is None:
                        ended = True
                        break
                    messages.append(value)
                for value in messages:
                    if rpc is None:
                        break
                    translated = normalize(value)
                    if translated:
                        if translated["method"] == "turn/completed":
                            rpc.close()
                            rpc = None
                        emit(translated)
                if ended and rpc:
                    raise rpc.failure or BridgeFailure("Muse guest process ended before the turn finished.", stage="guest exit")
    except Exception as error:
        diagnostic = failure(error, stage)
        if state:
            state.record_failure(diagnostic)
        raise diagnostic from error
    finally:
        if rpc:
            rpc.close()
        root.cleanup()


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["--guest"]:
            guest(guest_client, acknowledged=True)
        elif sys.argv[1:] == ["--mcp"]:
            mcp()
        elif sys.argv[1:2] == ["--account"]:
            print(json.dumps(read_account(Path(sys.argv[2]))), flush=True)
        elif sys.argv[1:2] == ["--artifact"]:
            print(artifact(Path(sys.argv[2])), flush=True)
        else:
            host()
    except Exception as error:
        diagnostic = failure(error, "adapter")
        print(json.dumps({"method": "bridge/failed", "params": diagnostic.details}), flush=True)
        sys.exit(1)
