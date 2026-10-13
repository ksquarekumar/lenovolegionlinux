#!/usr/bin/env python3
"""A stand-in remote MCP server (streamable HTTP, JSON answers) for the proxy test: `fake-remote-mcp.py <port>`.

Tools `docs` and `admin`; `docs` answers with the `x-api-key` header it received, so the test sees the key arrive.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOLS = [
    {"name": "docs", "description": "Look up docs", "inputSchema": {"type": "object"}},
    {"name": "admin", "description": "Not for agents", "inputSchema": {"type": "object"}},
]


class Server(BaseHTTPRequestHandler):
    def do_POST(self):
        message = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        method, ident = message.get("method"), message.get("id")
        if ident is None:
            self.send_response(202)
            self.end_headers()
            return
        if method == "initialize":
            result = {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-remote", "version": "1"},
            }
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            text = f"docs for {message['params'].get('arguments')} key={self.headers.get('x-api-key')}"
            result = {"content": [{"type": "text", "text": text}]}
        else:
            result = {}
        body = json.dumps({"jsonrpc": "2.0", "id": ident, "result": result}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("mcp-session-id", "s1")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # no server-to-client stream
        self.send_response(405)
        self.end_headers()

    def do_DELETE(self):
        self.send_response(200)
        self.end_headers()

    def log_message(self, format, *args):  # noqa: A002 (the base class names it so)
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Server).serve_forever()
