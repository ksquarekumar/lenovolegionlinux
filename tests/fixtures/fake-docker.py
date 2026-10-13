#!/usr/bin/env python3
"""A stand-in Docker Engine API on a unix socket for the virt test: `fake-docker.py <socket>`.

/version, /info (cgroupfs driver, no nvidia runtime) and /containers/json (a WinBoat container and another).
"""

import json
import os
import socketserver
import sys
from http.server import BaseHTTPRequestHandler

ANSWERS = {
    "/version": {"Version": "27.0.0"},
    "/info": {"DockerRootDir": "/var/lib/docker", "CgroupDriver": "cgroupfs", "Runtimes": {"runc": {}}},
    "/containers/json": [
        {"Names": ["/WinBoat"], "Image": "ghcr.io/dockur/windows:6.05", "Status": "Up 8 hours"},
        {"Names": ["/db"], "Image": "postgres:17", "Status": "Exited (0) 2 days ago"},
    ],
}


class Docker(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(ANSWERS.get(self.path.split("?")[0], {})).encode()
        self.send_response(200 if self.path.split("?")[0] in ANSWERS else 404)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def address_string(self):
        return "unix"

    def log_message(self, format, *args):  # noqa: A002 (the base class names it so)
        pass


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    pass


path = sys.argv[1]
if os.path.exists(path):
    os.unlink(path)
Server(path, Docker).serve_forever()
