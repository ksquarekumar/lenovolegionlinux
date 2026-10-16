#!/usr/bin/env python3
"""A stand-in usage endpoint for the account tests: `fake-usage.py <dir>`. It listens on a free port of 127.0.0.1
and writes the port to `<dir>/port` when it is ready.

Each GET answers with `<dir>/<access token>.json` ({"status": 200, "body": <JSON or text>, "headers": {...}}), read
at each call, so a test changes an account's answer by rewriting its file; no file: 404. Each call appends one JSON
line to `<dir>/calls.log`: the path, the token, and the User-Agent, anthropic-beta and ChatGPT-Account-ID headers.
"""

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

folder = sys.argv[1]


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        auth = self.headers.get("Authorization", "")
        token = auth.removeprefix("Bearer ")
        with open(os.path.join(folder, "calls.log"), "a") as log:
            log.write(json.dumps({
                "path": self.path, "token": token, "agent": self.headers.get("User-Agent"),
                "beta": self.headers.get("anthropic-beta"), "account": self.headers.get("ChatGPT-Account-ID"),
            }) + "\n")
        path = os.path.join(folder, token + ".json")
        if not token or "/" in token or not os.path.exists(path):
            self.send_response(404)
            self.end_headers()
            return
        with open(path) as f:
            answer = json.load(f)
        body = answer.get("body", {})
        data = (body if isinstance(body, str) else json.dumps(body)).encode()
        self.send_response(answer.get("status", 200))
        for name, value in answer.get("headers", {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(os.path.join(folder, "port.tmp"), "w") as f:
    f.write(str(server.server_address[1]))
os.rename(os.path.join(folder, "port.tmp"), os.path.join(folder, "port"))
server.serve_forever()
