"""Loopback provider transport fixture for the published Gateway lifecycle checks."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from queue import Queue
from threading import Thread
from uuid import uuid4


class ProviderFixture:
    provider = "ocm-fixture"
    model = "roundtrip"
    token = "ocm-fixture-key"

    def __init__(self):
        self.requests = Queue()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_arguments):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                fixture.requests.put((self.path, self.headers.get("Authorization"), body))
                chunks = [
                    {"role": "assistant", "content": fixture.reply},
                    {},
                ]
                events = []
                for index, delta in enumerate(chunks):
                    event = {
                        "id": "chatcmpl-ocm-fixture", "object": "chat.completion.chunk",
                        "created": 0, "model": fixture.model,
                        "choices": [{"index": 0, "delta": delta,
                                     "finish_reason": "stop" if index else None}],
                    }
                    events.append("data: " + json.dumps(event) + "\n\n")
                payload = ("".join(events) + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def configure(self, config_path):
        config = json.loads(config_path.read_text())
        defaults = config.setdefault("agents", {}).setdefault("defaults", {})
        defaults["model"] = {"primary": self.provider + "/" + self.model}
        config.setdefault("models", {})["providers"] = {
            self.provider: {
                "baseUrl": "http://127.0.0.1:" + str(self.server.server_port) + "/v1",
                "api": "openai-completions", "apiKey": self.token,
                "models": [{"id": self.model, "name": "OCM roundtrip fixture",
                            "contextWindow": 32768, "maxTokens": 1024}],
            },
        }
        config_path.write_text(json.dumps(config))

    def check(self, oc_json, env_name, stage):
        self.reply = "ocm-provider-reply-" + uuid4().hex
        question = "ocm-provider-request-" + uuid4().hex
        response = oc_json(
            "provider roundtrip " + stage, "@" + env_name, "--", "agent",
            "--agent", "main", "--session-id", uuid4().hex,
            "--message", question, "--thinking", "off", "--timeout", "60",
        )
        assert response["status"] == "ok", response
        assert [item.get("text") for item in response["result"]["payloads"]] == [self.reply]
        path, authorization, body = self.requests.get_nowait()
        assert path == "/v1/chat/completions", path
        assert authorization == "Bearer " + self.token
        assert body["model"] == self.model, body["model"]
        assert any(
            item["role"] == "user" and question in json.dumps(item["content"])
            for item in body["messages"]
        ), "provider did not receive this turn's message"
        assert self.requests.empty(), "unexpected additional provider requests"
        print("PASS: provider request, credential, model, message, and reply " + stage, flush=True)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), "provider fixture did not stop"
