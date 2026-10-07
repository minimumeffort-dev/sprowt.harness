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

from muse_transport import (Broker, CHECKSUM, MODEL, VERSION, Rpc, command_id,
                            guest, host_login, private_json, read_account, settings)


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
        raise RuntimeError("Install the supported Muse account CLI: " + VERSION)
    with urllib.request.urlopen(info["manifest_url"], timeout=30) as response:
        manifest = json.load(response)
    item = manifest["artifacts"]["aarch64_linux"]
    if (manifest.get("version") != VERSION or item.get("checksum") != CHECKSUM
            or not item.get("url", "").startswith("https://lookaside.facebook.com/")):
        raise RuntimeError("Muse release did not match the pinned Linux binary.")
    next_path = path.with_suffix(".next-" + secrets.token_hex(8))
    try:
        with urllib.request.urlopen(item["url"], timeout=60) as source, next_path.open("xb") as output:
            next_path.chmod(0o600)
            while chunk := source.read(1024 * 1024):
                output.write(chunk)
        if digest(next_path) != CHECKSUM or next_path.stat().st_size != item["size"]:
            raise RuntimeError("Muse binary checksum mismatch.")
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
        raise RuntimeError("Muse filesystem boundary includes controller source.")
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
        self.pool = threading.BoundedSemaphore(4)
        try:
            self.start(config)
        except (OSError, RuntimeError, ValueError, queue.Empty):
            self.broker.revoked = True
            self.executor.close()
            raise
        threading.Thread(target=self.read, daemon=True).start()

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
            raise RuntimeError("Muse requires an enforced guest process sandbox.")

    def envelope(self, value):
        with self.executor_lock:
            result = self.executor.call("process/write", {"processId": self.process_id,
                "writeId": secrets.token_hex(16),
                "chunk": base64.b64encode((json.dumps(value) + "\n").encode()).decode()})
            if result.get("status") != "accepted":
                raise RuntimeError("Guest stdin is unavailable.")

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
                        raise RuntimeError("Tool not advertised")
                    self.emit({"method": "item/tool/call", "id": "tool:" + value["id"], "params": params})
                    return
                self.envelope({"channel": "http", "id": value["id"], "status": 403, "done": True})
            else:
                self.broker.forward(value, lambda reply: self.envelope({"channel": "http", "id": value["id"], **reply}))
        except (OSError, RuntimeError, ValueError):
            if not self.stopped:
                self.envelope({"channel": "http", "id": value["id"], "status": 502, "done": True})
        finally:
            self.pool.release()

    def read(self):
        cursor, pending = 0, b""
        try:
            while not self.stopped:
                with self.executor_lock:
                    read = self.executor.call("process/read", {"processId": self.process_id, "afterSeq": cursor, "maxBytes": 65536, "waitMs": 20})
                for chunk in read["chunks"]:
                    if chunk["stream"] != "stdout":
                        continue
                    pending += base64.b64decode(chunk["chunk"])
                    if len(pending) > 4 * 1024 * 1024:
                        raise RuntimeError("Guest frame too large.")
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        value = json.loads(line)
                        if value.get("channel") == "http":
                            if not self.pool.acquire(blocking=False):
                                self.envelope({"channel": "http", "id": value["id"], "status": 429, "done": True})
                            else:
                                threading.Thread(target=self.relay, args=(value,), daemon=True).start()
                        elif value.get("channel") == "rpc":
                            self.incoming.put(value["value"])
                        else:
                            return
                cursor = max(cursor, read.get("nextSeq", 1) - 1)
                if read.get("closed"):
                    self.failure = "Muse guest exited before responding. Check the VM setup."
                    return
                time.sleep(0.01)
        except (OSError, RuntimeError, ValueError, queue.Empty):
            pass
        finally:
            self.incoming.put(None)

    def next(self, timeout=45):
        value = self.incoming.get(timeout=timeout)
        if value is None:
            raise RuntimeError(self.failure or "Muse guest ended before responding.")
        return value

    def close(self):
        self.broker.revoked = True
        self.stopped = True
        try:
            # Let the wrapper clean up native tools before terminating its boundary.
            with self.executor_lock:
                self.executor.call("process/write", {"processId": self.process_id, "writeId": secrets.token_hex(16), "chunk": base64.b64encode(b'{"channel":"stop"}\n').decode()})
            time.sleep(0.1)
            with self.executor_lock:
                self.executor.call("process/terminate", {"processId": self.process_id})
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
    rpc, config, commands, root = None, None, {}, tempfile.TemporaryDirectory(prefix="sprowt-muse-auth-")
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
                try:
                    if method == "turn/start":
                        if rpc:
                            rpc.close()
                        config = params["config"]
                        broker = Broker(headers, requests=128, lifetime=3600, output_tokens=8192)
                        auth = {"schema_version": 1, "providers": {"meta": {"api_key": broker.capability}}}
                        # Transfer only an expiring broker capability and advertised tool schemas.
                        guest_files = Path(root.name) / "guest"
                        guest_files.mkdir(exist_ok=True)
                        for name, data in [("auth.json", auth), (".sprowt-tools.json", params["tools"])]:
                            private_json(guest_files / name, data)
                            destination = config["env"]["HOME"] + ("/.config/muse/auth.json" if name == "auth.json" else "/" + name)
                            subprocess.run(["container", "copy", str(guest_files / name), config["name"] + ":" + destination], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        rpc = ManagedRpc(config, broker, emit, params["tools"])
                        init = rpc.call("initialize", {"clientInfo": {"name": "sprowt_harness", "version": "0.1"}, "capabilities": {"experimentalApi": True, "requestedCapabilities": ["sessionMcp"], "userInputDialogs": False}})
                        if "sessionMcp" not in init.get("grantedCapabilities", []):
                            raise RuntimeError("Muse did not enable session MCP tools.")
                        rpc.write({"method": "initialized"})
                        session = rpc.call("session/start", {"commandId": command_id(), "workspaceRoot": config["cwd"],
                            "providerId": "meta", "modelId": MODEL, "approvalMode": "allowAll",
                            "config": {"mcpServers": {"sprowt": {"transport": "stdio", "command": "/usr/bin/python3",
                                "args": ["/opt/sprowt-muse/bridge.py", "--mcp"], "framing": "lineDelimitedJson", "mode": "required"}}}})["session"]["sessionId"]
                        text = params["text"] + "\nReturn only a JSON object as your final answer matching this schema: " + json.dumps(params["schema"])
                        result = rpc.call("turn/start", {"commandId": commands.setdefault(params["source"], command_id()), "sessionId": session,
                            "reasoningEffort": "high", "input": [{"type": "text", "text": text}]})
                    elif method == "turn/steer":
                        result = rpc.call(method, {"commandId": commands.setdefault(params["source"], command_id()), "sessionId": session,
                            "expectedTurnId": params["turn"], "input": [{"type": "text", "text": text} for text in params["texts"]]})
                        if result.get("status") != "accepted":
                            raise RuntimeError("Muse did not accept steering.")
                        result = {"turnId": params["turn"]}
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
                        raise RuntimeError("Unsupported bridge method.")
                    emit({"id": message["id"], "result": result})
                except (OSError, RuntimeError, ValueError, queue.Empty, subprocess.SubprocessError) as error:
                    detail = str(error) if isinstance(error, RuntimeError) else type(error).__name__
                    emit({"id": message["id"], "error": {"code": -32000, "message": "Muse request failed: " + detail}})
                    if method == "turn/start" and rpc:
                        rpc.close()
                        rpc = None
            if rpc:
                messages, rpc.buffered = rpc.buffered, []
                while not rpc.incoming.empty():
                    value = rpc.incoming.get()
                    if value is None:
                        if not any(m.get("method") == "turn/completed" for m in messages):
                            raise RuntimeError("Muse guest process ended unexpectedly.")
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
    finally:
        if rpc:
            rpc.close()
        root.cleanup()


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["--guest"]:
            guest(guest_client)
        elif sys.argv[1:] == ["--mcp"]:
            mcp()
        elif sys.argv[1:2] == ["--account"]:
            print(json.dumps(read_account(Path(sys.argv[2]))), flush=True)
        elif sys.argv[1:2] == ["--artifact"]:
            print(artifact(Path(sys.argv[2])), flush=True)
        else:
            host()
    except (OSError, RuntimeError, ValueError, KeyError, queue.Empty):
        print(json.dumps({"method": "bridge/failed", "params": {"message": "Muse bridge failed. Verify the supported account login and VM setup."}}), flush=True)
        sys.exit(1)
