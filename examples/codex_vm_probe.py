#!/usr/bin/env python3
"""Disposable full-Codex VM experiment. Provider credentials stay on the Mac."""

import base64
import hashlib
import io
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.error
import urllib.request

from muse_vm_probe import IMAGE, NoRedirect, Rpc, boundary, container, guest

MODEL = "gpt-6.1-sol"
UPSTREAM = "https://chatgpt.com/backend-api/codex/responses"
VERSION = "0.159.2"
TOOL_HOST = "codex-code-mode-host-aarch64-unknown-linux-musl"
TOOL_HOST_CHECKSUM = "011967737e5eef730963dd63636e1006408ced43cbb3d6ca68c5c233998d6430"
CLIENT = {"name": "sprowt_codex_vm_probe", "title": "Sprowt VM probe", "version": "0.1"}


def tool_host(root):
    url = f"https://github.com/openai/codex/releases/download/rust-v{VERSION}/{TOOL_HOST}.tar.gz"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(32 * 1024 * 1024)
    if hashlib.sha256(data).hexdigest() != TOOL_HOST_CHECKSUM:
        raise RuntimeError("Codex tool-host archive checksum mismatch.")
    path = root / "codex-code-mode-host"
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        member = archive.getmember(TOOL_HOST)
        if not member.isfile() or member.size > 128 * 1024 * 1024:
            raise RuntimeError("Unexpected Codex tool-host archive member.")
        with archive.extractfile(member) as source, path.open("wb") as output:
            shutil.copyfileobj(source, output)
    path.chmod(0o755)
    return path


def host_login(root):
    login = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))) / "auth.json"
    home = root / "host-codex"
    home.mkdir(mode=0o700)
    (home / "auth.json").symlink_to(login.resolve(strict=True))
    env = {key: os.environ[key] for key in ("HOME", "PATH", "USER", "LANG", "TMPDIR")
           if key in os.environ}
    env["CODEX_HOME"] = str(home)
    rpc = Rpc(["codex", "app-server", "--listen", "stdio://", "-c",
               'cli_auth_credentials_store="file"'], env, root)
    try:
        rpc.call("initialize", {"clientInfo": CLIENT, "capabilities": {"experimentalApi": True}})
        rpc.write({"method": "initialized"})
        account = rpc.call("account/read", {"refreshToken": False})
        if (account.get("account") or {}).get("type") != "chatgpt":
            raise RuntimeError("A file-backed ChatGPT login is required; API keys are not used.")
        routing = account.get("workspaceRouting") or {}
        if routing and routing.get("backendOrigin") != "https://chatgpt.com":
            raise RuntimeError("This probe does not support alternate workspace routing.")
        catalog = rpc.call("model/list", {})
        if not any(item.get("model") == MODEL for item in catalog["data"]):
            raise RuntimeError("The selected model is absent from the host catalog.")
        usage = rpc.call("account/rateLimits/read", {"excludeResetCreditDetails": True})
        if usage.get("ordinaryUsageAllowed") is False:
            raise RuntimeError("Included account usage is currently unavailable.")
        auth = json.loads((home / "auth.json").read_text())
        tokens = auth.get("tokens") or {}
        if auth.get("auth_mode") not in (None, "chatgpt") or auth.get("OPENAI_API_KEY"):
            raise RuntimeError("This probe requires the existing ChatGPT account credential.")
        if not tokens.get("access_token") or not tokens.get("account_id"):
            raise RuntimeError("Host login lacks account-scoped access tokens.")
        print("Host ChatGPT login and model catalog verified; credentials stay in host memory.", flush=True)
        return tokens
    finally:
        rpc.close()


class Broker:
    def __init__(self, tokens):
        self.capability = secrets.token_hex(32)
        self.headers = {"Authorization": "Bearer " + tokens["access_token"],
                        "ChatGPT-Account-ID": tokens["account_id"],
                        "User-Agent": "sprowt_codex_vm_probe/0.1",
                        "Originator": CLIENT["name"], "Version": VERSION,
                        "Content-Type": "application/json", "Accept": "text/event-stream",
                        "Accept-Encoding": "identity"}
        self.secrets = [value.encode() for name, value in tokens.items()
                        if name in {"access_token", "refresh_token", "id_token"} and value]
        self.remaining = 8
        self.expires = time.monotonic() + 180
        self.revoked = False
        self.completed = 0
        self.metered = False
        self.lock = threading.Lock()
        self.opener = urllib.request.build_opener(NoRedirect())

    def allow(self, request):
        if not isinstance(request, dict) or any(not isinstance(request.get(key), str)
                                               for key in ("authorization", "method", "path", "body")):
            return False
        if self.revoked or time.monotonic() >= self.expires or self.remaining <= 0:
            return False
        if not secrets.compare_digest(request["authorization"].encode(), ("Bearer " + self.capability).encode()):
            return False
        if request["method"] != "POST" or request["path"] != "/responses":
            return False
        if not isinstance(request.get("responses_lite", False), bool):
            return False
        try:
            if len(request["body"]) > 3 * 1024 * 1024:
                return False
            raw = base64.b64decode(request["body"], validate=True)
            body = json.loads(raw)
            return (isinstance(body, dict) and len(raw) <= 2 * 1024 * 1024
                    and body.get("model") == MODEL and body.get("stream") is True
                    and body.get("store") is False
                    and body.get("reasoning", {}).get("effort") == "low")
        except (ValueError, TypeError, AttributeError):
            return False

    def forward(self, request, emit):
        with self.lock:
            if not self.allow(request):
                print("Broker rejected a request outside its model policy.", flush=True)
                emit({"status": 403, "done": True})
                return
            self.remaining -= 1
        headers = dict(self.headers)
        if request.get("responses_lite") is True:
            headers["x-openai-internal-codex-responses-lite"] = "true"
        upstream = urllib.request.Request(UPSTREAM, data=base64.b64decode(request["body"]),
                                          headers=headers, method="POST")
        try:
            with self.opener.open(upstream, timeout=45) as response:
                content_type = response.headers.get("Content-Type", "").split(";")[0].strip().lower()
                if content_type not in {"", "text/event-stream"}:
                    raise RuntimeError("Unexpected provider content type.")
                self.metered |= response.headers.get("x-codex-primary-used-percent") is not None
                total = 0
                started = False
                while line := response.readline(1024 * 1024):
                    total += len(line)
                    if (any(secret in line for secret in self.secrets)
                            or len(line) >= 1024 * 1024 or total > 4 * 1024 * 1024):
                        self.revoked = True
                        raise RuntimeError("Provider response failed the credential or size check.")
                    if self.revoked or time.monotonic() >= self.expires:
                        raise RuntimeError("Broker access ended.")
                    if not started:
                        if not line.strip():
                            continue
                        if not line.startswith((b"event:", b"data:", b":")):
                            raise RuntimeError("Unexpected provider content type.")
                        emit({"status": response.status, "content_type": "text/event-stream"})
                        started = True
                    emit({"chunk": base64.b64encode(line).decode()})
                if not started:
                    raise RuntimeError("Unexpected provider content type.")
                self.completed += 1
                emit({"done": True})
        except urllib.error.HTTPError as error:
            print(f"Provider HTTP {error.code}; response body withheld.", flush=True)
            emit({"status": error.code, "done": True})
        except (OSError, RuntimeError) as error:
            if not self.revoked:
                print("Broker transport failure:", type(error).__name__, flush=True)
            if not self.revoked and isinstance(error, RuntimeError) and str(error) in {
                "Unexpected provider content type.", "Provider response failed the credential or size check.", "Broker access ended."}:
                print(str(error), flush=True)
            emit({"status": 502, "done": True})


class VmRpc(Rpc):
    def __init__(self, name, broker):
        self.broker = broker
        self.lock = threading.Lock()
        self.requests = threading.BoundedSemaphore(2)
        super().__init__(["container", "exec", "--interactive", "--user", "1000:1000", name,
                         *boundary("/opt/probe"), "python3", "-u", "/opt/probe/codex_vm_probe.py", "--guest"])

    def envelope(self, value):
        with self.lock:
            self.process.stdin.write(json.dumps(value) + "\n")
            self.process.stdin.flush()

    def write(self, value):
        self.envelope({"channel": "rpc", "value": {"jsonrpc": "2.0", **value}})

    def forward(self, value):
        try:
            self.broker.forward(value, lambda reply: self.envelope(
                {"channel": "http", "id": value["id"], **reply}))
        except (OSError, ValueError):
            pass
        finally:
            self.requests.release()

    def read(self):
        try:
            for line in self.process.stdout:
                if len(line) > 3 * 1024 * 1024:
                    break
                value = json.loads(line)
                if value.get("channel") == "rpc":
                    self.incoming.put(value["value"])
                elif value.get("channel") == "http" and self.requests.acquire(blocking=False):
                    threading.Thread(target=self.forward, args=(value,), daemon=True).start()
                elif value.get("channel") == "http":
                    self.envelope({"channel": "http", "id": value["id"], "status": 429, "done": True})
                else:
                    break
        except (ValueError, OSError):
            pass
        finally:
            self.incoming.put(None)


def codex_client(base):
    home = Path.home() / ".codex"
    home.mkdir(mode=0o700)
    config = f'''model = "{MODEL}"
model_provider = "sprowt_broker"
model_reasoning_effort = "low"
approval_policy = "never"
sandbox_mode = "danger-full-access"
cli_auth_credentials_store = "ephemeral"
web_search = "disabled"
allow_login_shell = false
[model_providers.sprowt_broker]
name = "Sprowt account broker"
base_url = "{base}"
env_key = "SPROWT_BROKER_TOKEN"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
[features]
apps = false
plugins = false
hooks = false
multi_agent = false
browser_use = false
computer_use = false
image_generation = false
shell_snapshot = false
enable_request_compression = false
[analytics]
enabled = false
[feedback]
enabled = false
'''
    (home / "config.toml").write_text(config)
    env = {"HOME": str(Path.home()), "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
           "CODEX_HOME": str(home), "SPROWT_BROKER_TOKEN": (Path.home() / "broker-token").read_text()}
    return subprocess.Popen(["codex", "app-server", "--listen", "stdio://", "--strict-config"],
                            cwd="/tasks/1", env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)


def next_event(rpc, deadline):
    while time.monotonic() < deadline:
        if rpc.buffered:
            return rpc.buffered.pop(0)
        try:
            return rpc.next(min(30, deadline - time.monotonic()))
        except queue.Empty:
            pass
    raise RuntimeError("VM turn timed out.")


def finished(rpc, turn, status, timeout=45):
    deadline = time.monotonic() + timeout
    while True:
        message = next_event(rpc, deadline)
        if message.get("method") == "turn/completed" and message["params"]["turn"]["id"] == turn:
            if message["params"]["turn"]["status"] != status:
                raise RuntimeError("VM turn ended with an unexpected status; details withheld.")
            return


def vm_probe(root, broker, helper):
    name = "sprowt-codex-vm-" + secrets.token_hex(8)
    rpc = None
    try:
        container("run", "--detach", "--name", name, "--cpus", "2", "--memory", "2G",
                  "--masked-path", "NONE", "--read-only-path", "NONE", IMAGE)
        config = json.loads(container("inspect", name))[0]["configuration"]
        if config["mounts"] or config["publishedPorts"]:
            raise RuntimeError("Probe VM has unexpected mounts or ports.")
        print("Disposable VM started without host mounts or ports.", flush=True)
        setup = '''set -eu
sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources
apt-get update -qq
apt-get install -y --no-install-recommends python3 >/dev/null
mkdir -p /opt/probe /tasks/1 /tasks/other /home/worker
printf 'synthetic sibling fixture' >/tasks/other/fixture.txt
chown -R 1000:1000 /tasks/1 /home/worker
'''
        container("exec", name, "sh", "-c", setup)
        if container("exec", name, "codex", "--version").strip() != b"codex-cli 0.159.2":
            raise RuntimeError("The VM CLI does not match the pinned version.")
        container("copy", str(helper), f"{name}:/usr/local/bin/codex-code-mode-host")
        container("exec", name, "chmod", "755", "/usr/local/bin/codex-code-mode-host")
        for filename in ("codex_vm_probe.py", "muse_vm_probe.py"):
            container("copy", str(Path(__file__).with_name(filename).resolve()), f"{name}:/opt/probe/{filename}")
        token = root / "guest-broker-token"
        token.write_text(broker.capability)
        token.chmod(0o600)
        container("copy", str(token), f"{name}:/home/worker/broker-token")
        container("exec", name, "chown", "-R", "1000:1000", "/home/worker")
        check = '''from pathlib import Path
import socket
assert not Path('/tasks/other/fixture.txt').exists()
assert not Path('/root/.codex/auth.json').exists()
Path('/tasks/1/boundary.txt').write_text('isolated')
try: socket.create_connection(('1.1.1.1',443),timeout=1)
except OSError: pass
else: raise AssertionError('unexpected external network')
'''
        container("exec", "--user", "1000:1000", name, *boundary("/opt/probe"), "python3", "-c", check)
        print("Whole-process filesystem and network boundary verified; starting Linux Codex.", flush=True)
        broker.expires = time.monotonic() + 180
        rpc = VmRpc(name, broker)
        rpc.call("initialize", {"clientInfo": CLIENT, "capabilities": {"experimentalApi": True}})
        rpc.write({"method": "initialized"})
        print("Linux Codex JSON-RPC initialized.", flush=True)
        info = rpc.call("environment/info", {"environmentId": "local"})
        if info.get("cwd") != "file:///tasks/1":
            raise RuntimeError("The CLI did not register its guest-local executor.")
        preflight = rpc.call("command/exec", {
            "command": ["/bin/bash", "-c", "printf native-preflight"],
            "cwd": "/tasks/1", "sandboxPolicy": {"type": "dangerFullAccess"}})
        if preflight.get("exitCode") != 0 or preflight.get("stdout") != "native-preflight":
            raise RuntimeError("Guest-local native executor returned unexpected output.")
        thread = rpc.call("thread/start", {"model": MODEL, "cwd": "/tasks/1",
                         "approvalPolicy": "never", "sandbox": "danger-full-access",
                         "environments": [{"environmentId": "local", "cwd": "/tasks/1"}],
                         "ephemeral": True, "developerInstructions": "This is a disposable probe. Do not delegate, install packages, use network or read credentials. Be brief."})["thread"]["id"]
        prompt = ("Create hello.py that prints exactly 'hello sprowt'. Run it with python3 and verify output. "
                  "Use a native command to attempt reading /tasks/other/fixture.txt, report denied when absent, "
                  "and continue. Do not read any other files. Keep your final reply short.")
        turn = rpc.call("turn/start", {"threadId": thread, "effort": "low",
                        "input": [{"type": "text", "text": prompt}]})["turn"]["id"]
        steered = rpc.call("turn/steer", {"threadId": thread, "expectedTurnId": turn,
                          "input": [{"type": "text", "text": "Finish with sprowt-steering-ok."}]})
        deadline = time.monotonic() + 150
        native = denied = marker = False
        while True:
            message = next_event(rpc, deadline)
            params = message.get("params", {})
            if message.get("method") == "item/completed":
                item = params["item"]
                if item.get("type") == "commandExecution":
                    native |= item.get("exitCode") == 0
                    denied |= "/tasks/other/fixture.txt" in item.get("command", "") and (
                        item.get("exitCode") != 0 or "No such file" in (item.get("aggregatedOutput") or ""))
                    print("VM native command finished; output withheld.", flush=True)
                if item.get("type") == "agentMessage":
                    marker |= "sprowt-steering-ok" in item.get("text", "")
            if message.get("method") == "turn/completed" and params["turn"]["id"] == turn:
                if params["turn"]["status"] != "completed":
                    raise RuntimeError("Codex VM model turn failed; details withheld.")
                break
        if not (native and denied and marker and steered.get("turnId") == turn):
            raise RuntimeError("Native execution, sibling denial, or steering was not verified.")
        if container("exec", name, "python3", "/tasks/1/hello.py").strip() != b"hello sprowt":
            raise RuntimeError("Independent output check failed.")
        print("VM edit, independent execution, sibling read denial and steering verified.", flush=True)

        cancel = rpc.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text":
            "Run python3 -c 'import time; time.sleep(30)' once, then finish. This checks cancellation."}]})["turn"]["id"]
        deadline = time.monotonic() + 60
        while True:
            message = next_event(rpc, deadline)
            if message.get("method") == "item/started" and message["params"]["item"].get("type") == "commandExecution":
                break
            if message.get("method") == "turn/completed":
                raise RuntimeError("Cancellation fixture did not start a native command.")
        rpc.call("turn/interrupt", {"threadId": thread, "turnId": cancel})
        finished(rpc, cancel, "interrupted")
        rpc.call("thread/backgroundTerminals/clean", {"threadId": thread})
        stopped = '''from pathlib import Path
import time
deadline = time.monotonic() + 5
while True:
    running = False
    for path in Path('/proc').glob('[0-9]*/cmdline'):
        try: args = path.read_bytes().split(b'\\0')
        except OSError: continue
        running |= b'import time; time.sleep(30)' in args
    if not running: break
    assert time.monotonic() < deadline, 'Cancelled child still running'
    time.sleep(0.1)
'''
        container("exec", name, "python3", "-c", stopped)
        print("VM turn interrupted; worker terminals cleaned and child process exit verified.", flush=True)

        broker.revoked = True
        failed = rpc.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text":
            "Create never-created.txt containing unexpected. This checks a revoked broker."}]})["turn"]["id"]
        finished(rpc, failed, "failed")
        container("exec", name, "test", "!", "-e", "/tasks/1/never-created.txt")
        exported = container("exec", name, "tar", "-cf", "-", "/home/worker", "/tasks/1")
        if any(secret in exported for secret in broker.secrets):
            raise RuntimeError("Provider credential found in exported guest files.")
        print(f"PASS: {broker.completed} ChatGPT-authenticated requests; no provider credential in guest files.")
        print("Revoked connection fails closed; no fallback execution or new file.")
        print("Account usage response headers observed:", broker.metered)
    finally:
        broker.revoked = True
        if rpc:
            rpc.close()
        container("delete", "--force", name)
        print("Disposable VM deleted; broker access revoked.", flush=True)


def main():
    if sys.argv[1:] == ["--guest"]:
        return guest(codex_client)
    if sys.argv[1:]:
        raise RuntimeError("Usage: python3 examples/codex_vm_probe.py")
    if subprocess.check_output(["codex", "--version"], text=True).strip() != "codex-cli " + VERSION:
        raise RuntimeError("This probe requires Codex CLI " + VERSION + ".")
    with tempfile.TemporaryDirectory(prefix="sprowt-codex-vm-") as directory:
        root = Path(directory)
        root.chmod(0o700)
        helper = tool_host(root)
        vm_probe(root, Broker(host_login(root)), helper)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(f"BLOCKED: {error}")
        raise SystemExit(1)
    except (OSError, ValueError, KeyError, queue.Empty):
        print("BLOCKED: process or transport failed. No credentials were logged.")
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("Cancelled; probe cleanup ran.")
        raise SystemExit(130)
