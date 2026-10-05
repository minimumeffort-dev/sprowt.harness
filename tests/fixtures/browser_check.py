import errno
import http.server
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import urllib.error
import urllib.request


peer_task, peer_home = sys.argv[1:]
assert Path.cwd().as_posix().startswith("/tasks/")
assert os.environ["HOME"].startswith("/home/sprowt/workers/")
assert Path(peer_task).is_dir() and Path(peer_home).is_dir()
for folder in ("/workspace", peer_task, peer_home, "/usr/local/bin"):
    try:
        Path(folder, "browser-denied").write_text("denied")
    except OSError:
        pass
    else:
        raise AssertionError(f"Write escaped task boundary: {folder}")
for file in (".git", "/opt/sprowt-git/repo/config"):
    assert Path(file).is_file()
    try:
        with open(file, "ab"):
            pass
    except OSError:
        pass
    else:
        raise AssertionError(f"Git metadata is writable: {file}")
assert not Path("/opt/codex-home/auth.json").exists()
assert not any(os.environ.get(key) for key in ("OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"))

with tempfile.TemporaryDirectory() as runtime:
    path = str(Path(runtime, "local.sock"))
    with socket.socket(socket.AF_UNIX) as listener, socket.socket(socket.AF_UNIX) as client:
        listener.bind(path)
        listener.listen()
        client.connect(path)
        with listener.accept()[0] as peer:
            client.sendall(b"hello")
            assert peer.recv(5) == b"hello"

for family in (socket.AF_NETLINK, getattr(socket, "AF_VSOCK", 40)):
    try:
        unexpected = socket.socket(family, socket.SOCK_STREAM)
    except OSError as error:
        assert error.errno == errno.EPERM, error
    else:
        unexpected.close()
        raise AssertionError(f"Unexpected socket family allowed: {family}")

try:
    urllib.request.urlopen("http://sprowt-policy-check.invalid", timeout=5)
except urllib.error.HTTPError as error:
    assert error.code == 403, error
else:
    raise AssertionError("Unlisted domain allowed")
for address in (("140.82.112.3", 443), ("192.168.64.1", 80)):
    try:
        connection = socket.create_connection(address, timeout=1)
    except OSError:
        pass
    else:
        connection.close()
        raise AssertionError(f"Direct connection allowed: {address}")


class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        html = b"""<!doctype html><style>
        body { margin: 0 } button { width: 100%; outline: 3px solid green }
        </style><button>focus</button><script>
        const button = document.querySelector('button'); button.focus();
        document.body.dataset.checked = document.activeElement === button
          && button.getBoundingClientRect().width === innerWidth
          && getComputedStyle(button).outlineWidth === '3px' ? 'passed' : 'failed';
        </script>"""
        self.send_response(200)
        self.end_headers()
        self.wfile.write(html)

    def log_message(self, *args):
        pass


with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page) as server:
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory() as profile:
            browser = subprocess.run(
                ["/usr/bin/chromium", "--headless", "--no-sandbox", "--disable-dev-shm-usage",
                 f"--user-data-dir={profile}", "--window-size=375,800", "--dump-dom",
                 f"http://127.0.0.1:{server.server_port}"],
                capture_output=True, text=True, timeout=20,
            )
            assert browser.returncode == 0, browser.stderr
            assert 'data-checked="passed"' in browser.stdout, browser.stdout
    finally:
        server.shutdown()
        thread.join()

print("browser and boundary checks passed")
