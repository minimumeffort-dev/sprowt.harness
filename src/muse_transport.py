#!/usr/bin/env python3
"""Muse account authentication and credential-free stdio transport."""

import base64
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import urllib.error
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from http.client import HTTPException

MODEL = "muse-spark-1.3"
ORIGIN = "https://api.meta.ai"
VERSION = "1.4.3-R5018.1"
CHECKSUM = "6426c76a0081f20d60f6cad03308a147d79ce45758f1a89fd2713253cf475497"
IMAGE = "sprowt-sandbox:0.159.2-v1"


class BridgeFailure(RuntimeError):
    """Only bounded, credential-free diagnostics cross the bridge."""
    def __init__(self, message, **details):
        self.details = {"message": message, **details}
        super().__init__(message)


def failure(error, stage):
    if isinstance(error, BridgeFailure):
        return error
    if isinstance(error, queue.Empty):
        reason = "response timed out"
    elif isinstance(error, RpcError):
        reason = str(error)
    elif isinstance(error, OSError):
        reason = f"I/O failed (errno {error.errno})"
    else:
        # Exception strings can include request bodies, paths or credentials.
        reason = type(error).__name__
    return BridgeFailure(f"Muse {stage}: {reason}.", stage=stage)


def provider_failure(error, secrets_to_hide):
    detail, code = "", None
    try:
        raw = error.read(8193)
        body = json.loads(raw) if len(raw) <= 8192 else {}
        value = body.get("error", {}) if isinstance(body, dict) else {}
        if isinstance(value, dict):
            code = value.get("code")
            detail = " · ".join(str(value[k]) for k in ("code", "message")
                                if isinstance(value.get(k), (str, int)))
    except (OSError, ValueError, HTTPException):
        pass
    for secret in secrets_to_hide:
        if secret:
            for encoded in (secret, base64.b64encode(secret.encode()).decode(),
                            urllib.parse.quote(secret, safe="")):
                detail = detail.replace(encoded, "[redacted]")
    detail = re.sub(r"https?://\S+", "[url]", detail)
    detail = re.sub(r"(?i)(?:bearer\s+|(?:authorization|api[_-]?key|access[_-]?token|password)[\"']?\s*[:=]\s*[\"']?)[^\s,;\"']+",
                    "[redacted]", detail)
    detail = re.sub(r"[A-Za-z0-9_+/=.-]{40,}", "[redacted]", detail)
    detail = " ".join(detail.split())
    detail = "".join(c for c in detail if c.isprintable())[:400]
    context = error.code in (400, 413) and code in ("context_length_exceeded", "max_context_length_exceeded")
    hint = ("Ctrl+R starts a fresh conversation from saved work." if context else
            "Run muse login on your Mac, then Ctrl+R." if error.code == 401 else
            "Request rejected; automatic retries stopped.")
    return BridgeFailure(f"Muse provider HTTP {error.code}" + (f" · {detail}" if detail else "")
                         + f". {hint}", stage="provider", http_status=error.code,
                         recovery="fresh_session" if context else "retry")


def command_id():
    value = (int(time.time() * 1000) << 80) | secrets.randbits(80)
    return (
        f"{value >> 96:08x}-{(value >> 80) & 0xffff:04x}-7{(value >> 64) & 0xfff:03x}-"
        f"8{(value >> 48) & 0xfff:03x}-{value & 0xffffffffffff:012x}")


def settings(base):
    value = {
        "schema_version": 1,
        "endpoint_transport": {"base_url": base, "auth": "bearer"},
        "context": {"foreign_personal_rules": False, "foreign_personal_skills": False},
        "run": {"subagent_delegation_mode": "off", "toolset": []},
    }
    value["runtime_capabilities"] = {
        f"plugin:tbh-reminders:reminder:{name}": {"enabled": False}
        for name in ("memory", "skill-reminder", "todo-reminder", "goal-reminder",
                     "verify-reminder", "scope-reminder")
    }
    return value


def private_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))
    path.chmod(0o600)


class RpcError(RuntimeError):
    def __init__(self, method, error):
        self.method = method
        self.kind = (error.get("data") or {}).get("kind")
        code = error.get("code")
        code = code if isinstance(code, int) else "unknown"
        super().__init__(f"CLI rejected {method} ({code}); details withheld.")


class Rpc:
    def __init__(self, command, env=None, cwd=None, own_group=True):
        self.own_group = own_group
        self.process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, start_new_session=own_group)
        self.incoming = queue.Queue()
        self.counter = 0
        self.buffered = []
        self.failure = None
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            for line in self.process.stdout:
                value = json.loads(line)
                if not isinstance(value, dict):
                    raise ValueError("Expected an RPC object")
                self.incoming.put(value)
        except (ValueError, OSError) as error:
            self.failure = failure(error, "CLI output")
        finally:
            self.incoming.put(None)

    def write(self, value):
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", **value}) + "\n")
        self.process.stdin.flush()

    def next(self, timeout=45):
        value = self.incoming.get(timeout=timeout)
        if value is None:
            code = self.process.poll()
            raise self.failure or BridgeFailure(f"Muse CLI process ended (code {code}).", stage="CLI exit", exit_code=code)
        return value

    def call(self, method, params):
        self.counter += 1
        self.write({"id": self.counter, "method": method, "params": params})
        while True:
            value = self.next()
            if value.get("id") == self.counter and "method" not in value:
                if "error" in value:
                    raise RpcError(method, value["error"])
                return value["result"]
            if "id" in value and "method" in value:
                self.write({"id": value["id"], "error": {"code": -32601,
                            "message": "No host tools in this probe"}})
            else:
                self.buffered.append(value)

    def close(self):
        if self.process.poll() is None:
            try:
                if self.own_group:
                    os.killpg(self.process.pid, signal.SIGKILL)
                else:
                    self.process.kill()
            except ProcessLookupError:
                pass
        self.process.wait()
        try:
            self.process.stdin.close()
        except OSError:
            pass


def account_client(root, base, own_group=True):
    config = root / "config/muse"
    auth = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "muse/auth.json"
    private_json(config / "settings.json", settings(base))
    (config / "auth.json").symlink_to(auth)
    env = {key: os.environ[key] for key in ("HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG")
           if key in os.environ}
    env.update(XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
               XDG_CACHE_HOME=str(root / "cache"), MUSE_NO_AUTO_UPDATE="1")
    return Rpc(["muse", "serve", "--disable-shell", "--disable-write"], env, root, own_group)


def read_account(root):
    # Stay in the caller's process group so a discovery timeout stops both processes.
    rpc = account_client(root, "http://127.0.0.1:9", own_group=False)
    try:
        rpc.call("initialize", {"clientInfo": {"name": "sprowt_account_check", "version": "0.1"},
                                "capabilities": {"experimentalApi": True}})
        rpc.write({"method": "initialized"})
        return {"state": rpc.call("account/read", {}).get("state", "unknown")}
    finally:
        rpc.close()


def host_login(root, quiet=False):
    """Let the genuine CLI resolve its account credential into host memory."""
    captured = {}

    class Capture(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            captured.update({k.lower(): v for k, v in self.headers.items()})
            catalog = {"object": "list", "data": [{"id": MODEL, "object": "model",
                       "metadata": {"muse-code": {"release_date": "2026-01-01",
                       "is_hidden": False, "limit": {"context": 1000000, "output": 1024}}}}]}
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(catalog).encode())

        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            captured.clear()
            captured.update({k.lower(): v for k, v in self.headers.items()})
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            response = {"id": "resp_probe", "object": "response", "model": MODEL,
                        "status": "completed", "output": [],
                        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}
            for value in [
                {"type": "response.created", "sequence_number": 1, "response": response},
                {"type": "response.output_text.delta", "sequence_number": 2,
                 "output_index": 0, "item_id": "msg_probe", "content_index": 0, "delta": "sprowt-auth-probe"},
                {"type": "response.completed", "sequence_number": 3, "response": response},
            ]:
                self.wfile.write(("data: " + json.dumps(value) + "\n\n").encode())

    server = ThreadingHTTPServer(("127.0.0.1", 0), Capture)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    rpc = None
    try:
        rpc = account_client(root, f"http://127.0.0.1:{server.server_port}")
        rpc.call("initialize", {"clientInfo": {"name": "sprowt_muse_broker_probe", "version": "0.1"},
                                "capabilities": {"experimentalApi": True}})
        rpc.write({"method": "initialized"})
        account = rpc.call("account/read", {})
        if account.get("state") != "accountLogin":
            raise RuntimeError("A Muse account login is required; API keys are not used.")
        rpc.call("model/list", {"providerId": "meta"})
        session = rpc.call("session/start", {"commandId": command_id(),
                           "workspaceRoot": str(root), "providerId": "meta", "modelId": MODEL})
        session_id = session["session"]["sessionId"]
        turn = rpc.call("turn/start", {"commandId": command_id(), "sessionId": session_id,
                        "reasoningEffort": "low", "input": [{"type": "text", "text": "Reply sprowt-auth-probe."}]})
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            message = rpc.buffered.pop(0) if rpc.buffered else rpc.next()
            if message.get("method") == "turn/completed" and message["params"]["turnId"] == turn["turnId"]:
                if message["params"]["terminal"] != "completed":
                    raise RuntimeError("Host credential resolution turn failed.")
                break
        else:
            raise RuntimeError("Host credential resolution timed out.")
        if not captured.get("authorization", "").startswith("Bearer "):
            raise RuntimeError("BLOCKED: Muse did not authenticate through the configured endpoint.")
        if not quiet:
            print("Host account login: verified; genuine request authentication stays in memory.")
        return captured
    finally:
        if rpc:
            rpc.close()
        server.shutdown()
        server.server_close()
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


class Broker:
    def __init__(self, headers, requests=8, lifetime=180, output_tokens=2048):
        self.capability = secrets.token_hex(32)
        self.headers = {key: value for key, value in headers.items()
                        if key in {"authorization", "user-agent", "x-client-id"}}
        self.secret = headers["authorization"].removeprefix("Bearer ").encode()
        self.remaining = requests
        self.expires = time.monotonic() + lifetime
        self.output_tokens = output_tokens
        self.guard = threading.Lock()
        self.revoked = False
        self.completed = 0
        self.failure = None
        self.on_failure = lambda _: None
        self.opener = urllib.request.build_opener(NoRedirect())

    def allow(self, request):
        if not isinstance(request, dict) or any(not isinstance(request.get(key, ""), str)
                                               for key in ("authorization", "method", "path", "body")):
            return False
        if self.revoked or time.monotonic() >= self.expires or self.remaining <= 0:
            return False
        if not secrets.compare_digest(request.get("authorization", ""), "Bearer " + self.capability):
            return False
        if request.get("method") == "GET" and request.get("path") == "/muse-code/models":
            return not request.get("body")
        if request.get("method") != "POST" or request.get("path") != "/responses":
            return False
        try:
            if len(request.get("body", "")) > 3 * 1024 * 1024:
                return False
            raw = base64.b64decode(request.get("body", ""), validate=True)
            body = json.loads(raw)
            return (isinstance(body, dict) and len(raw) <= 2 * 1024 * 1024
                    and body.get("model") == MODEL and body.get("stream") is True)
        except (ValueError, TypeError):
            return False

    def forward(self, request, emit):
        with self.guard:
            if not self.allow(request):
                emit({"status": 403, "done": True})
                return
            self.remaining -= 1
        data = base64.b64decode(request["body"]) if request["method"] == "POST" else None
        if data is not None:
            body = json.loads(data)
            body["max_output_tokens"] = self.output_tokens
            data = json.dumps(body).encode()
        headers = {**self.headers, "Content-Type": "application/json", "Accept-Encoding": "identity"}
        route = "/v1/responses" if data is not None else "/muse-code/models"
        upstream = urllib.request.Request(ORIGIN + route, data=data,
                                          headers=headers, method=request["method"])
        try:
            with self.opener.open(upstream, timeout=60) as response:
                content_type = response.headers.get("Content-Type", "").split(";")[0]
                if content_type not in {"application/json", "text/event-stream"}:
                    raise RuntimeError("Unexpected provider content type.")
                emit({"status": response.status, "content_type": content_type})
                total = 0
                while line := response.readline(1024 * 1024):
                    total += len(line)
                    if self.secret in line or len(line) >= 1024 * 1024 or total > 4 * 1024 * 1024:
                        self.revoked = True
                        raise RuntimeError("Provider response failed the credential or size check.")
                    emit({"chunk": base64.b64encode(line).decode()})
                if request["method"] == "POST":
                    self.completed += 1
                emit({"done": True})
        except urllib.error.HTTPError as error:
            if 400 <= error.code < 500 and error.code not in (408, 429):
                diagnostic = provider_failure(error, [self.secret.decode(), self.capability])
                with self.guard:
                    self.failure = diagnostic
                    self.revoked = True
                self.on_failure(diagnostic)
            emit({"status": error.code, "done": True})
        except (OSError, RuntimeError):
            emit({"status": 502, "done": True})


FRAME_BYTES = 32 * 1024
MESSAGE_BYTES = 4 * 1024 * 1024


class FramedOutput:
    """Backpressure keeps unread output below exec-server's retention limit."""
    def __init__(self):
        self.writer = threading.Lock()
        self.condition = threading.Condition()
        self.sequence = self.acknowledged = 0
        self.stopped = False

    def acknowledge(self, sequence):
        with self.condition:
            if type(sequence) is int and sequence == self.sequence:
                self.acknowledged = sequence
                self.condition.notify_all()

    def close(self):
        with self.condition:
            self.stopped = True
            self.condition.notify_all()

    def send(self, value):
        data = json.dumps(value).encode()
        if len(data) > MESSAGE_BYTES:
            raise BridgeFailure("Muse guest message exceeded the 4 MiB limit.", stage="guest output")
        with self.writer:
            for offset in range(0, len(data), FRAME_BYTES):
                with self.condition:
                    if self.stopped:
                        raise BridgeFailure("Muse output transport closed.", stage="guest output")
                    self.sequence += 1
                    print(json.dumps({"channel": "frame", "seq": self.sequence,
                        "chunk": base64.b64encode(data[offset:offset + FRAME_BYTES]).decode(),
                        "last": offset + FRAME_BYTES >= len(data)}), flush=True)
                    if not self.condition.wait_for(
                            lambda: self.acknowledged == self.sequence or self.stopped, timeout=45):
                        raise BridgeFailure("Muse output acknowledgment timed out.", stage="guest output")
                    if self.stopped:
                        raise BridgeFailure("Muse output transport closed.", stage="guest output")


class FrameReader:
    def __init__(self):
        self.sequence = 0
        self.pending = bytearray()

    def receive(self, frame):
        if (frame.get("channel") != "frame" or type(frame.get("seq")) is not int
                or frame["seq"] != self.sequence + 1 or type(frame.get("last")) is not bool
                or not isinstance(frame.get("chunk"), str)
                or len(frame["chunk"]) > 4 * ((FRAME_BYTES + 2) // 3)):
            raise BridgeFailure("Muse VM output frame is missing or invalid.", stage="VM transport")
        try:
            chunk = base64.b64decode(frame["chunk"], validate=True)
            if not chunk or len(chunk) > FRAME_BYTES or len(self.pending) + len(chunk) > MESSAGE_BYTES:
                raise ValueError()
            self.pending.extend(chunk)
            self.sequence = frame["seq"]
            if not frame["last"]:
                return None
            value = json.loads(self.pending)
            self.pending.clear()
            if not isinstance(value, dict):
                raise ValueError()
            return value
        except (ValueError, TypeError):
            raise BridgeFailure("Muse VM message is incomplete, invalid or too large.", stage="VM transport") from None


def guest(start_client, acknowledged=False):
    """A credential-free HTTP tunnel and CLI share the isolated network namespace."""
    replies = {}
    lock = threading.Lock()
    output = FramedOutput() if acknowledged else None

    def emit(value):
        if output:
            output.send(value)
        else:
            with lock:
                print(json.dumps(value), flush=True)

    class Tunnel(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def relay(self):
            size = int(self.headers.get("Content-Length", "0"))
            if size > 2 * 1024 * 1024:
                self.send_error(413)
                return
            request_id = secrets.token_hex(16)
            incoming = queue.Queue()
            replies[request_id] = incoming
            try:
                emit({"channel": "http", "id": request_id, "method": self.command,
                      "path": self.path, "authorization": self.headers.get("Authorization", ""),
                      "responses_lite": self.headers.get("x-openai-internal-codex-responses-lite") == "true",
                      "body": base64.b64encode(self.rfile.read(size)).decode()})
                started = False
                while True:
                    reply = incoming.get(timeout=960 if self.path == "/sprowt/tools" else 70)
                    if not started:
                        self.send_response(reply.get("status", 502))
                        self.send_header("Content-Type", reply.get("content_type", "application/json"))
                        self.end_headers()
                        started = True
                    if "chunk" in reply:
                        self.wfile.write(base64.b64decode(reply["chunk"]))
                        self.wfile.flush()
                    if reply.get("done"):
                        break
            finally:
                replies.pop(request_id, None)

        do_GET = relay
        do_POST = relay
        do_DELETE = relay

    server = ThreadingHTTPServer(("127.0.0.1", 0), Tunnel)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    client = start_client(f"http://127.0.0.1:{server.server_port}")

    def read():
        try:
            for line in client.stdout:
                emit({"channel": "rpc", "value": json.loads(line)})
            emit({"channel": "ended", "exit_code": client.wait()})
        except (OSError, ValueError, BridgeFailure) as error:
            try:
                emit({"channel": "failed", **failure(error, "guest output").details})
            except (OSError, BridgeFailure):
                pass

    threading.Thread(target=read, daemon=True).start()
    incoming = queue.Queue()

    def read_input():
        try:
            for line in sys.stdin:
                message = json.loads(line)
                if output and message.get("channel") == "ack":
                    output.acknowledge(message.get("seq"))
                else:
                    incoming.put(message)
                    if message.get("channel") == "stop":
                        break
        except (OSError, ValueError, AttributeError):
            incoming.put({"channel": "stop"})
        finally:
            if output:
                output.close()
            incoming.put(None)

    # Acknowledgments must flow even when a large request blocks CLI stdin.
    threading.Thread(target=read_input, daemon=True).start()
    try:
        while (message := incoming.get()) is not None:
            if message.get("channel") == "stop":
                break
            if message.get("channel") == "rpc":
                client.stdin.write(json.dumps(message["value"]) + "\n")
                client.stdin.flush()
            elif message.get("channel") == "http" and message["id"] in replies:
                replies[message["id"]].put(message)
    finally:
        if output:
            output.close()
        client.kill()
        client.wait()
        server.shutdown()
        server.server_close()
