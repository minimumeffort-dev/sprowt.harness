#!/usr/bin/env python3
"""Disposable Muse credential-broker experiment. Never logs credentials."""

import hashlib
import json
from pathlib import Path
import queue
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from muse_transport import (Broker, CHECKSUM, IMAGE, MODEL, VERSION, NoRedirect, Rpc,
                            command_id, guest, host_login, private_json, settings)


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
        raise RuntimeError("This probe requires installed Muse " + VERSION + ".")
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


def boundary(directory="/opt/muse"):
    return ["bwrap", "--unshare-all", "--unshare-user", "--disable-userns",
            "--die-with-parent", "--new-session", "--cap-drop", "ALL",
            "--ro-bind", "/usr", "/usr", "--ro-bind", "/lib", "/lib",
            "--ro-bind", "/etc", "/etc", "--symlink", "usr/bin", "/bin",
            "--symlink", "usr/sbin", "/sbin", "--proc", "/proc", "--dev", "/dev",
            "--tmpfs", "/tmp", "--ro-bind", directory, directory,
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


def muse_client(base):
    config = Path.home() / ".config/muse"
    value = settings(base)
    value["run"].pop("toolset")
    private_json(config / "settings.json", value)
    return subprocess.Popen(["/opt/muse/muse", "serve", "--disable-sandbox"],
                            cwd="/tasks/1", stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)


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
        container("copy", str(Path(__file__).resolve().parents[1] / "src/muse_transport.py"), f"{name}:/opt/muse/muse_transport.py")
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
        return guest(muse_client)
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
