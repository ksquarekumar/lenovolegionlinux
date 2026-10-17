#!/usr/bin/env python3
"""An ACP agent that replays a recorded live transcript (argv[1]: the adapter's stdout lines, one JSON-RPC message
each), so a test drives legionctl's ACP client with a real adapter's answer shape.

The recording holds, in order: the initialize answer, notifications, the session/new answer, the session/set_model
answer, the session/update notifications of one prompt and the prompt's answer. Each request gets the recorded
answer of its kind (by order: initialize, session/new, session/set_model, session/prompt) with the request's id;
before the prompt's answer come all notifications after the session/new answer. Another request gets an empty
result. Like the real adapters, it does not end when its input closes.
"""

import json
import sys
import threading

recorded = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
answers = [m for m in recorded if "id" in m and "method" not in m]
kinds = ["initialize", "session/new", "session/set_model", "session/prompt"]
answer_of = dict(zip(kinds, answers))
after_new = recorded.index(answer_of["session/new"])
prompt_updates = [m for m in recorded[after_new + 1:] if "method" in m]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    message = json.loads(line)
    method, ident = message.get("method"), message.get("id")
    if ident is None:
        continue
    if method == "session/prompt":
        for update in prompt_updates:
            send(update)
    if method in answer_of:
        send({**answer_of[method], "id": ident})
    else:
        send({"jsonrpc": "2.0", "id": ident, "result": {}})
    if method == "initialize":
        for note in recorded[1:after_new]:
            if "method" in note:
                send(note)

threading.Event().wait()
