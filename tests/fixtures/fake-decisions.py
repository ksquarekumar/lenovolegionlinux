#!/usr/bin/env python3
"""A stand-in decision server (rlcd, todo-138): `fake-decisions.py <port file> <log>`; it writes its port to the file.

The first path segment picks the answer: http://127.0.0.1:<port>/<choice>/api/alpha/decisions answers in the System
One shape (questions keyed by id), .../<choice>/v1/decisions in the OpenAI shape (answers in question order). A choice
question gets <choice> (else its first option) with probability 0.8 and 0.1 for each other option, confidence 0.6; a
yes-no question 0.02 (System One noul) or 0.3 (OpenAI predicate); a score 0.5. /fail/... answers 500 as the OpenAI
preview did on 2026-10-16; /slow/... answers after 3 s. Each call appends `{"path", "auth", "body"}` to the log.
"""

import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file, log = sys.argv[1], sys.argv[2]


def choice(options, pick):
    chosen = pick if pick in options else options[0]
    probabilities = {o: (0.8 if o == chosen else 0.1) for o in options}
    return {"type": "choice", "choice": chosen, "probabilities": probabilities, "confidence": 0.6}


def system_one(body, pick):
    answers = {}
    for qid, q in body["questions"].items():
        if q["type"] == "choice":
            answers[qid] = choice(list(q["criteria"]), pick)
        elif q["type"] == "noul":
            answers[qid] = {"type": "noul", "noul": 0.02}
        else:
            answers[qid] = {"type": "score", "score": 0.5}
    return {"model": body["model"], "answers": answers,
            "usage": {"input_tokens": 169, "output_tokens": 0, "cost": 0.000007}, "id": "dec-1", "provider": "stand-in"}


def openai(body, pick):
    answers = []
    for q in body["questions"]:
        if q["type"] == "choice":
            answers.append(choice([c["value"] for c in q["choices"]], pick))
        elif q["type"] == "predicate":
            answers.append({"type": "predicate", "probability": 0.3})
        else:
            answers.append({"type": "score", "score": 0.5})
    return {"model": body["model"], "answers": answers, "usage": {"input_tokens": 42, "cost": 0.0000042}}


class Decisions(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        with open(log, "a") as f:
            f.write(json.dumps({"path": self.path, "auth": self.headers.get("authorization"), "body": body}) + "\n")
        pick = self.path.split("/")[1]
        if pick == "slow":
            time.sleep(3)
        if pick == "fail":
            status, answer = 500, {"error": {"message": "Decision admission timed out.", "type": "server_error"}}
        else:
            status, answer = 200, (openai if self.path.endswith("/v1/decisions") else system_one)(body, pick)
        payload = json.dumps(answer).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, _format, *_args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Decisions)
with open(port_file + ".new", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(port_file + ".new", port_file)
server.serve_forever()
