#!/usr/bin/env python3
"""Disposable Muse credential-broker experiment. Never logs credentials."""

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
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "muse-spark-1.3"
ORIGIN = "https://api.meta.ai"
VERSION = "1.4.1-R4503.1"
CHECKSUM = "a6d46239975adac282aa829d2a5bd1cd3119334c18ecfa776d4377daebddb595"
IMAGE = "sprowt-sandbox:0.159.2-v1"


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


class Rpc:
    def __init__(self, command, env=None, cwd=None):
        self.process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, start_new_session=True)
        self.incoming = queue.Queue()
        self.counter = 0
        self.buffered = []
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            for line in self.process.stdout:
                self.incoming.put(json.loads(line))
        except (ValueError, OSError):
            pass
        finally:
            self.incoming.put(None)

    def write(self, value):
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", **value}) + "\n")
        self.process.stdin.flush()

    def next(self, timeout=45):
        value = self.incoming.get(timeout=timeout)
        if value is None:
            raise RuntimeError("Muse process ended before responding.")
        return value

    def call(self, method, params):
        self.counter += 1
        self.write({"id": self.counter, "method": method, "params": params})
        while True:
            value = self.next()
            if value.get("id") == self.counter and "method" not in value:
                if "error" in value:
                    code = value["error"].get("code")
                    code = code if isinstance(code, int) else "unknown"
                    raise RuntimeError(f"Muse rejected {method} ({code}); details withheld.")
                return value["result"]
            if "id" in value and "method" in value:
                self.write({"id": value["id"], "error": {"code": -32601,
                            "message": "No host tools in this probe"}})
            else:
                self.buffered.append(value)

    def close(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGKILL)
        self.process.wait()


def host_login(root):
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
    config = root / "config/muse"
    auth = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "muse/auth.json"
    private_json(config / "settings.json", settings(f"http://127.0.0.1:{server.server_port}"))
    (config / "auth.json").symlink_to(auth)
    env = {key: os.environ[key] for key in ("HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG")
           if key in os.environ}
    env.update(XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
               XDG_CACHE_HOME=str(root / "cache"), MUSE_NO_AUTO_UPDATE="1")
    rpc = Rpc(["muse", "serve", "--disable-shell", "--disable-write"], env, root)
    try:
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
        print("Host account login: verified; genuine request authentication stays in memory.")
        return captured
    finally:
        rpc.close()
        server.shutdown()
        server.server_close()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


class Broker:
    def __init__(self, headers):
        self.capability = secrets.token_hex(32)
        self.headers = {key: value for key, value in headers.items()
                        if key in {"authorization", "user-agent", "x-client-id"}}
        self.secret = headers["authorization"].removeprefix("Bearer ").encode()
        self.remaining = 8
        self.expires = time.monotonic() + 180
        self.revoked = False
        self.completed = 0
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
        if not self.allow(request):
            emit({"status": 403, "done": True})
            return
        self.remaining -= 1
        data = base64.b64decode(request["body"]) if request["method"] == "POST" else None
        if data is not None:
            body = json.loads(data)
            body["max_output_tokens"] = 2048
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
            print(f"Provider HTTP {error.code}; response body withheld.")
            emit({"status": error.code, "done": True})
        except (OSError, RuntimeError):
            emit({"status": 502, "done": True})


def container(*args, input=None):
    result = subprocess.run(["container", *args], input=input, capture_output=True, timeout=180)
    if result.returncode:
        for line in result.stderr.decode(errors="replace").splitlines():
            if line.startswith("bwrap:"):
                print(line)
        raise RuntimeError(f"Container {args[0]} failed ({result.returncode}); output withheld.")
    return result.stdout


def artifact(root):
    if len(sys.argv) == 3 and sys.argv[1] == "--binary":
        path = Path(sys.argv[2]).resolve()
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != CHECKSUM:
            raise RuntimeError("Supplied Linux binary checksum mismatch.")
        return path
    info = json.loads((Path.home() / ".local/bin/.muse-release-info.json").read_text())
    if info.get("version") != VERSION:
        raise RuntimeError("This probe requires installed Muse 1.4.1-R4503.1.")
    manifest_url = info["manifest_url"]
    if not manifest_url.startswith("https://lookaside.facebook.com/"):
        raise RuntimeError("Unrecognized Muse release origin.")
    with urllib.request.urlopen(manifest_url, timeout=30) as response:
        manifest = json.load(response)
    item = manifest["artifacts"]["aarch64_linux"]
    if manifest.get("version") != VERSION or item.get("checksum") != CHECKSUM:
        raise RuntimeError("Linux release manifest does not match the pinned binary.")
    if not item["url"].startswith("https://lookaside.facebook.com/"):
        raise RuntimeError("Unrecognized Muse artifact origin.")
    digest = hashlib.sha256()
    path = root / "muse"
    print("Downloading the pinned Linux CLI; no account credential is sent.", flush=True)
    with urllib.request.urlopen(item["url"], timeout=60) as response, path.open("wb") as output:
        while chunk := response.read(1024 * 1024):
            digest.update(chunk)
            output.write(chunk)
    if digest.hexdigest() != CHECKSUM or path.stat().st_size != item["size"]:
        raise RuntimeError("Linux binary checksum or size mismatch.")
    path.chmod(0o755)
    return path


def boundary():
    return ["bwrap", "--unshare-all", "--unshare-user", "--disable-userns",
            "--die-with-parent", "--new-session", "--cap-drop", "ALL",
            "--ro-bind", "/usr", "/usr", "--ro-bind", "/lib", "/lib",
            "--ro-bind", "/etc", "/etc", "--symlink", "usr/bin", "/bin",
            "--symlink", "usr/sbin", "/sbin", "--proc", "/proc", "--dev", "/dev",
            "--tmpfs", "/tmp", "--ro-bind", "/opt/muse", "/opt/muse",
            "--bind", "/tasks/1", "/tasks/1", "--bind", "/home/worker", "/home/worker",
            "--chdir", "/tasks/1", "--clearenv", "--setenv", "HOME", "/home/worker",
            "--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin", "--setenv", "LANG", "C.UTF-8"]


class VmRpc(Rpc):
    def __init__(self, name, broker):
        self.broker = broker
        self.lock = threading.Lock()
        super().__init__(["container", "exec", "--interactive", "--user", "1000:1000", name,
                         *boundary(), "python3", "-u", "/opt/muse/probe.py", "--guest"])

    def envelope(self, value):
        with self.lock:
            self.process.stdin.write(json.dumps(value) + "\n")
            self.process.stdin.flush()

    def write(self, value):
        self.envelope({"channel": "rpc", "value": {"jsonrpc": "2.0", **value}})

    def read(self):
        try:
            for line in self.process.stdout:
                value = json.loads(line)
                if value.get("channel") == "rpc":
                    self.incoming.put(value["value"])
                elif value.get("channel") == "http":
                    self.broker.forward(value, lambda reply: self.envelope(
                        {"channel": "http", "id": value["id"], **reply}))
                else:
                    self.incoming.put(None)
                    break
        except (ValueError, OSError):
            pass
        finally:
            self.incoming.put(None)


def guest():
    """A credential-free HTTP tunnel and Muse share the isolated network namespace."""
    replies = {}
    lock = threading.Lock()

    def emit(value):
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
                      "body": base64.b64encode(self.rfile.read(size)).decode()})
                started = False
                while True:
                    reply = incoming.get(timeout=70)
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
    config = Path.home() / ".config/muse"
    value = settings(f"http://127.0.0.1:{server.server_port}")
    value["run"].pop("toolset")
    private_json(config / "settings.json", value)
    muse = subprocess.Popen(["/opt/muse/muse", "serve", "--disable-sandbox"],
                            cwd="/tasks/1", stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)

    def read():
        for line in muse.stdout:
            emit({"channel": "rpc", "value": json.loads(line)})

    threading.Thread(target=read, daemon=True).start()
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if message.get("channel") == "rpc":
                muse.stdin.write(json.dumps(message["value"]) + "\n")
                muse.stdin.flush()
            elif message.get("channel") == "http" and message["id"] in replies:
                replies[message["id"]].put(message)
    finally:
        muse.kill()
        muse.wait()
        server.shutdown()
        server.server_close()


def vm_probe(root, broker, binary):
    name = "sprowt-muse-vm-" + secrets.token_hex(8)
    rpc = None
    try:
        container("run", "--detach", "--name", name, "--cpus", "2", "--memory", "2G",
                  "--masked-path", "NONE", "--read-only-path", "NONE", IMAGE)
        config = json.loads(container("inspect", name))[0]["configuration"]
        if config["mounts"] or config["publishedPorts"]:
            raise RuntimeError("Probe VM has unexpected mounts or ports.")
        print("Disposable VM started without host mounts or ports.", flush=True)
        setup = """set -eu
sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources
apt-get update -qq
apt-get install -y --no-install-recommends python3 >/dev/null
mkdir -p /opt/muse /tasks/1 /tasks/other /home/worker/.config/muse
printf 'synthetic sibling fixture' >/tasks/other/fixture.txt
chown -R 1000:1000 /tasks/1 /home/worker
"""
        container("exec", name, "sh", "-c", setup)
        print("VM runtime setup complete.", flush=True)
        container("copy", str(binary), f"{name}:/opt/muse/muse")
        container("exec", name, "chmod", "755", "/opt/muse/muse")
        container("copy", str(Path(__file__).resolve()), f"{name}:/opt/muse/probe.py")
        auth = root / "guest-auth.json"
        private_json(auth, {"schema_version": 1, "providers": {"meta": {"api_key": broker.capability}}})
        container("copy", str(auth), f"{name}:/home/worker/.config/muse/auth.json")
        container("exec", name, "chown", "-R", "1000:1000", "/home/worker")
        check = """from pathlib import Path
import socket
assert not Path('/tasks/other/fixture.txt').exists()
assert not Path('/root/.config/muse/auth.json').exists()
Path('/tasks/1/boundary.txt').write_text('isolated')
try:
 socket.create_connection(('1.1.1.1',443),timeout=1)
except OSError: pass
else: raise AssertionError('unexpected external network')
print('Whole-process filesystem and network boundary verified.')
"""
        container("exec", "--user", "1000:1000", name, *boundary(), "python3", "-c", check)
        print("Whole-process boundary verified; testing the genuine Linux CLI.", flush=True)
        broker.expires = time.monotonic() + 180
        rpc = VmRpc(name, broker)
        initialized = rpc.call("initialize", {"clientInfo": {"name": "sprowt_muse_vm_probe", "version": "0.1"},
                                              "capabilities": {"experimentalApi": True}})
        rpc.write({"method": "initialized"})
        print("Linux Muse version:", initialized["serverInfo"]["version"], flush=True)
        session = rpc.call("session/start", {"commandId": command_id(), "workspaceRoot": "/tasks/1",
                           "providerId": "meta", "modelId": MODEL, "approvalMode": "allowAll"})["session"]["sessionId"]
        prompt = ("This is a disposable VM test. Create hello.py that prints exactly 'hello sprowt', "
                  "then run it with python3 and verify its output. Attempt native read_file on "
                  "/tasks/other/fixture.txt and report denied when absent. Do not read other files "
                  "or credentials, use network, install packages, or delegate. Keep the final reply short.")
        turn = rpc.call("turn/start", {"commandId": command_id(), "sessionId": session,
                        "reasoningEffort": "low", "input": [{"type": "text", "text": prompt}]})["turnId"]
        steering = rpc.call("turn/steer", {"commandId": command_id(), "sessionId": session,
                            "expectedTurnId": turn,
                            "input": [{"type": "text", "text": "Finish with sprowt-steering-ok."}]})
        deadline = time.monotonic() + 180
        completed = False
        steered = False
        denied = False
        native = False
        while time.monotonic() < deadline:
            message = rpc.buffered.pop(0) if rpc.buffered else rpc.next(30)
            params = message.get("params", {})
            if message.get("method") == "item/completed":
                item = params["item"]
                if item.get("kind") == "toolCall":
                    print("VM tool:", item.get("tool"), item.get("status"), flush=True)
                    native |= item.get("status") == "completed" and item.get("tool") in {"bash", "shell", "write_file", "apply_patch"}
                    denied |= item.get("tool") == "read_file" and item.get("status") == "failed"
                if item.get("kind") == "agentMessage":
                    steered |= "sprowt-steering-ok" in item.get("text", "")
            if message.get("method") == "turn/completed" and params["turnId"] == turn:
                completed = params["terminal"] == "completed"
                break
        if not (completed and native and denied and steered and steering.get("status") == "accepted"):
            raise RuntimeError("Muse VM completion, native tools, read denial, or steering was not verified.")
        if container("exec", name, "python3", "/tasks/1/hello.py").strip() != b"hello sprowt":
            raise RuntimeError("Independent output check failed.")
        broker.revoked = True
        exported = container("exec", name, "tar", "-cf", "-", "/home/worker", "/tasks/1")
        if broker.secret in exported:
            raise RuntimeError("Provider credential found in exported guest files.")
        if broker.allow({"method": "GET", "path": "/muse-code/models",
                         "authorization": "Bearer " + broker.capability}):
            raise RuntimeError("Revoked broker still allows model access.")
        print(f"PASS: {broker.completed} account-authenticated model requests; VM edit and independent execution verified.")
        print("Guest files contain no provider credential; steering and sibling read denial verified.")
        print("Subscription metering is not independently exposed by this CLI; no extra API key was used.")
    finally:
        broker.revoked = True
        if rpc:
            rpc.close()
        container("delete", "--force", name)
        print("Disposable VM deleted; broker access revoked.", flush=True)


def main():
    if sys.argv[1:] == ["--guest"]:
        return guest()
    if sys.argv[1:] and not (len(sys.argv) == 3 and sys.argv[1] == "--binary"):
        raise RuntimeError("Usage: python3 examples/muse_vm_probe.py [--binary PATH]")
    with tempfile.TemporaryDirectory(prefix="sprowt-muse-vm-") as directory:
        root = Path(directory)
        root.chmod(0o700)
        broker = Broker(host_login(root))
        vm_probe(root, broker, artifact(root))


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(f"BLOCKED: {error}")
        raise SystemExit(1)
    except (OSError, queue.Empty):
        print("BLOCKED: process or transport failed. No credentials were logged.")
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("Cancelled; probe cleanup ran.")
        raise SystemExit(130)
