import base64
import io
import json
from pathlib import Path
import sys
import time
import unittest
import urllib.error

from codex_vm_probe import Broker, MODEL, UPSTREAM
from muse_vm_probe import Rpc


class BrokerTests(unittest.TestCase):
    def setUp(self):
        self.broker = Broker({"access_token": "synthetic-access-secret",
                              "refresh_token": "synthetic-refresh-secret",
                              "id_token": "synthetic-id-secret", "account_id": "test-account"})

    def request(self, **changes):
        body = {"model": MODEL, "stream": True, "store": False, "reasoning": {"effort": "low"}}
        return {"method": "POST", "path": "/responses",
                "body": base64.b64encode(json.dumps(body).encode()).decode(),
                "authorization": "Bearer " + self.broker.capability, "responses_lite": True, **changes}

    def test_only_scoped_requests_are_allowed(self):
        self.assertTrue(self.broker.allow(self.request()))
        for changes in [{"authorization": "Bearer other-worker"}, {"method": "GET"},
                        {"authorization": "Bearer invalid\u00e9"},
                        {"path": "/account"}, {"path": UPSTREAM}, {"path": "/%72esponses"},
                        {"responses_lite": "invalid"},
                        {"path": "/responses?url=https://attacker.invalid"}, {"body": "broken"}]:
            with self.subTest(changes=changes):
                self.assertFalse(self.broker.allow(self.request(**changes)))
        for changes in [{"model": "other"}, {"stream": False}, {"store": True},
                        {"reasoning": {"effort": "xhigh"}}, {"reasoning": None}]:
            body = json.loads(base64.b64decode(self.request()["body"])) | changes
            self.assertFalse(self.broker.allow(self.request(
                body=base64.b64encode(json.dumps(body).encode()).decode())))

    def test_expiry_budget_and_revocation_stop_access(self):
        for attribute, value in [("expires", time.monotonic() - 1), ("remaining", 0), ("revoked", True)]:
            previous = getattr(self.broker, attribute)
            setattr(self.broker, attribute, value)
            self.assertFalse(self.broker.allow(self.request()))
            setattr(self.broker, attribute, previous)

    def upstream(self, data, content_type="text/event-stream"):
        class Response(io.BytesIO):
            status = 200
            headers = {"Content-Type": content_type, "Set-Cookie": "never-forward",
                       "x-codex-primary-used-percent": "1.0"}

        class Upstream:
            request = None

            def open(inner, request, timeout):
                inner.request = request
                return Response(data)

        self.broker.opener = Upstream()

    def test_account_authentication_is_injected_only_upstream(self):
        self.upstream(b"data: harmless\n\n")
        output = []
        self.broker.forward(self.request(), output.append)
        request = self.broker.opener.request
        self.assertEqual(request.full_url, UPSTREAM)
        self.assertEqual(request.get_header("Authorization"), "Bearer synthetic-access-secret")
        self.assertEqual(request.get_header("Chatgpt-account-id"), "test-account")
        self.assertEqual(request.get_header("X-openai-internal-codex-responses-lite"), "true")
        self.assertNotIn(self.broker.capability, str(request.headers))
        self.assertNotIn("never-forward", json.dumps(output))
        self.assertEqual(self.broker.completed, 1)
        self.assertTrue(self.broker.metered)

    def test_all_provider_tokens_are_withheld(self):
        for secret in self.broker.secrets:
            self.broker.revoked = False
            self.upstream(b"data: " + secret + b"\n\n")
            output = []
            self.broker.forward(self.request(), output.append)
            self.assertNotIn(secret.decode(), json.dumps(output))
            self.assertTrue(self.broker.revoked)

    def test_transport_failure_does_not_return_provider_details(self):
        class Failed:
            def open(inner, request, timeout):
                raise urllib.error.URLError("synthetic-access-secret")

        self.broker.opener = Failed()
        output = []
        self.broker.forward(self.request(), output.append)
        self.assertEqual(output, [{"status": 502, "done": True}])

    def test_missing_mime_header_requires_an_sse_frame(self):
        self.upstream(b"event: response.created\ndata: {}\n\n", content_type="")
        output = []
        self.broker.forward(self.request(), output.append)
        self.assertEqual(output[0]["status"], 200)
        self.upstream(b"<html>unexpected</html>\n", content_type="")
        output = []
        self.broker.forward(self.request(), output.append)
        self.assertEqual(output, [{"status": 502, "done": True}])


class TunnelTests(unittest.TestCase):
    def test_shared_transport_relays_http_rpc_and_cli_exit(self):
        fake = '''import json,sys,urllib.request
message = json.loads(sys.stdin.readline())
request = urllib.request.Request(sys.argv[1] + '/responses', data=b'{}', headers={
    'Authorization':'Bearer synthetic-worker-token',
    'x-openai-internal-codex-responses-lite':'true', 'X-Untrusted':'ignore'})
with urllib.request.urlopen(request, timeout=5) as response:
    print(json.dumps({'id':message['id'],'result':response.read().decode()}), flush=True)
'''
        program = f'''import subprocess,sys
from muse_vm_probe import guest
def start(base):
    return subprocess.Popen([sys.executable,'-c',{fake!r},base], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, text=True)
guest(start)
'''
        rpc = Rpc([sys.executable, "-u", "-c", program], cwd=Path(__file__).parent)

        def send(value):
            rpc.process.stdin.write(json.dumps(value) + "\n")
            rpc.process.stdin.flush()

        try:
            send({"channel": "rpc", "value": {"id": 7, "method": "test", "params": {}}})
            request = rpc.next(timeout=5)
            self.assertEqual(request["channel"], "http")
            self.assertEqual(request["authorization"], "Bearer synthetic-worker-token")
            self.assertTrue(request["responses_lite"])
            self.assertNotIn("ignore", json.dumps(request))
            send({"channel": "http", "id": request["id"], "status": 200,
                  "content_type": "text/event-stream", "done": True,
                  "chunk": base64.b64encode(b"data: harmless\n\n").decode()})
            self.assertEqual(rpc.next(timeout=5), {
                "channel": "rpc", "value": {"id": 7, "result": "data: harmless\n\n"}})
            self.assertEqual(rpc.next(timeout=5), {"channel": "ended", "exit_code": 0})
        finally:
            rpc.close()
            rpc.process.stdout.close()


if __name__ == "__main__":
    unittest.main()
