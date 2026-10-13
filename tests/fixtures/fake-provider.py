#!/usr/bin/env python3
"""A stand-in model provider for the gateway test: `fake-provider.py <port> <log>`.

Base URLs pick the behaviour: http://127.0.0.1:<port>/ok answers (JSON, or an SSE stream when the request asks for
one) with usage 10 in / 5 out; /limited answers 429. Each call appends `<prefix> <model> <x-api-key>` to the log.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port, log = int(sys.argv[1]), sys.argv[2]


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        prefix = self.path.split("/")[1]
        with open(log, "a") as f:
            f.write(f"{prefix} {body.get('model')} {self.headers.get('x-api-key')}\n")
        if prefix == "limited":
            self.send_response(429)
            self.send_header("content-type", "application/json")
            self.send_header("retry-after", "30")
            self.end_headers()
            self.wfile.write(b'{"type":"error","error":{"type":"rate_limit_error"}}')
            return
        text = f"model={body.get('model')} key={self.headers.get('x-api-key')}"
        if body.get("stream"):
            events = [
                {"type": "message_start", "message": {"model": body.get("model"), "usage": {"input_tokens": 10}}},
                {"type": "content_block_delta", "delta": {"type": "text_delta", "text": text}},
                {"type": "message_delta", "usage": {"output_tokens": 5}},
            ]
            payload = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
        else:
            payload = json.dumps(
                {"model": body.get("model"), "content": [{"type": "text", "text": text}],
                 "usage": {"input_tokens": 10, "output_tokens": 5}}
            ).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):  # noqa: A002 (the base class names it so)
        pass


ThreadingHTTPServer(("127.0.0.1", port), Provider).serve_forever()
