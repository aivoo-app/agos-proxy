"""Mock OpenAI-compatible upstream so the Docker test env needs no real API key."""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _prompt(self, req):
        text = ""
        for m in req.get("messages", []):
            c = m.get("content")
            if isinstance(c, str):
                text += c
            else:
                text += " ".join(
                    p.get("text", "") for p in c if isinstance(p, dict)
                )
        return text

    def _sse(self, req, model, text):
        """Emit a minimal OpenAI-style SSE stream with chunked deltas."""
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

        def chunk(delta, finish=None):
            return json.dumps({
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{"index": 0, "finish_reason": finish,
                             "delta": delta}],
            })

        for word in text.split(" "):
            self.wfile.write(b"data: " + chunk(
                {"role": "assistant", "content": word + " "}).encode() + b"\n\n")
            self.wfile.flush()
        self.wfile.write(b"data: " + chunk({}, "stop").encode() + b"\n\n")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        model = req.get("model", "mock-model")
        if self.path == "/v1/chat/completions":
            if req.get("stream"):
                self._sse(req, model, "mock reply to: " + self._prompt(req))
                return
            self._send(200, {
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": model,
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant",
                                         "content": "mock reply to: " + self._prompt(req)}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 4, "total_tokens": 5},
            })
        elif self.path == "/v1/completions":
            self._send(200, {
                "id": "cmpl-mock", "object": "text_completion", "model": model,
                "choices": [{"index": 0, "finish_reason": "stop", "text": "mock completion"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
            })
        elif self.path == "/v1/embeddings":
            self._send(200, {
                "object": "list", "model": model,
                "data": [{"object": "embedding", "index": 0,
                          "embedding": [0.1, 0.2, 0.3]}],
                "usage": {"prompt_tokens": 1, "total_tokens": 1},
            })
        else:
            self._send(404, {"error": {"message": "unknown path"}})

    def do_GET(self):
        if self.path == "/v1/models":
            self._send(200, {"object": "list", "data": [{"id": "mock-model"}]})
        else:
            self._send(404, {"error": {"message": "unknown path"}})

    def log_message(self, fmt, *args):
        print("mock:", fmt % args, flush=True)


HTTPServer(("0.0.0.0", 9999), Handler).serve_forever()
