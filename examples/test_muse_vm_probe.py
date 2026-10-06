import base64
import io
import json
import time
import unittest

from muse_vm_probe import Broker, MODEL


class BrokerTests(unittest.TestCase):
    def setUp(self):
        self.broker = Broker({"authorization": "Bearer synthetic-provider-secret",
                              "user-agent": "muse-test", "x-client-id": "test-client"})

    def request(self, **changes):
        body = base64.b64encode(json.dumps({"model": MODEL, "stream": True}).encode()).decode()
        return {"method": "POST", "path": "/responses", "body": body,
                "authorization": "Bearer " + self.broker.capability, **changes}

    def test_only_scoped_model_requests_are_allowed(self):
        self.assertTrue(self.broker.allow(self.request()))
        self.assertTrue(self.broker.allow(self.request(method="GET", path="/muse-code/models", body="")))
        for changes in [
            {"authorization": "Bearer other-worker"}, {"method": "DELETE"},
            {"path": "/account"}, {"path": "https://attacker.invalid/responses"},
            {"path": "/responses?url=https://attacker.invalid"}, {"path": "/%72esponses"},
            {"body": "not-base64"},
            {"body": base64.b64encode(b'{"model":"other-model","stream":true}').decode()},
            {"body": base64.b64encode(json.dumps({"model": MODEL, "stream": False}).encode()).decode()},
        ]:
            with self.subTest(changes=changes):
                self.assertFalse(self.broker.allow(self.request(**changes)))

    def test_expiry_budget_and_revocation_stop_access(self):
        for attribute, value in [("expires", time.monotonic() - 1), ("remaining", 0), ("revoked", True)]:
            previous = getattr(self.broker, attribute)
            setattr(self.broker, attribute, value)
            self.assertFalse(self.broker.allow(self.request()))
            setattr(self.broker, attribute, previous)

    def test_provider_credential_is_injected_only_upstream_and_not_returned(self):
        class Response(io.BytesIO):
            status = 200
            headers = {"Content-Type": "text/event-stream", "Set-Cookie": "never-forward"}

        class Upstream:
            request = None

            def open(inner, request, timeout):
                inner.request = request
                return Response(b"data: synthetic-provider-secret\n\n")

        self.broker.opener = Upstream()
        output = []
        self.broker.forward(self.request(), output.append)
        upstream = self.broker.opener.request
        self.assertEqual(upstream.full_url, "https://api.meta.ai/v1/responses")
        self.assertEqual(upstream.get_header("Authorization"), "Bearer synthetic-provider-secret")
        self.assertEqual(json.loads(upstream.data)["max_output_tokens"], 2048)
        self.assertNotIn("synthetic-provider-secret", json.dumps(output))
        self.assertNotIn("never-forward", json.dumps(output))
        self.assertTrue(self.broker.revoked)


if __name__ == "__main__":
    unittest.main()
