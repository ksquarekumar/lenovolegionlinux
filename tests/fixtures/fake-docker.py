#!/usr/bin/env python3
"""A stand-in Docker Engine API on a unix socket for the virt tests: `fake-docker.py <socket> [<log>]`.

/version, /info (cgroupfs driver, no nvidia runtime) and /containers/json (a WinBoat container and another);
POST /containers/<name>/<action> appends `<action> <name>` to the log and answers 204, or 404 for another name.
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

    def do_POST(self):
        _, _, name, action = self.path.split("/")
        names = [c["Names"][0].lstrip("/") for c in ANSWERS["/containers/json"]]
        if name not in names:
            body = json.dumps({"message": f"No such container: {name}"}).encode()
            self.send_response(404)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        with open(sys.argv[2], "a") as f:
            f.write(f"{action} {name}\n")
        self.send_response(204)
        self.end_headers()

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
