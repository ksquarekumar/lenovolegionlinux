#!/usr/bin/env python3
"""A stand-in ACP agent (protocol v1, newline-delimited JSON-RPC on stdio) for the headless-run test.

On a prompt it asks one permission, commits a file and tries a push in its working folder (inside the session's
sandbox), then answers with what it saw: the prompt, the MCP servers from session/new, the permission outcome, and
whether the commit and the push went through. A prompt that starts with SLOW waits 30 s first (a long run, for the
test that stops the operator in the middle of one).
"""

import json
import os
import subprocess
import sys
import time

mcp_servers = []
pending = {}
next_id = 100


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def request(method, params):
    """Send a request to the client and wait for its answer (answering nothing else meanwhile)."""
    global next_id
    next_id += 1
    send({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params})
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("id") == next_id and "method" not in message:
            return message.get("result", {})
    return {}


def run(argv):
    done = subprocess.run(argv, capture_output=True, text=True)
    return done.returncode == 0, done.stdout + done.stderr


def prompt(params):
    text = " ".join(block.get("text", "") for block in params.get("prompt", []))
    if text.startswith("SLOW"):
        time.sleep(30)
    outcome = request(
        "session/request_permission",
        {
            "sessionId": params["sessionId"],
            "toolCall": {"toolCallId": "t1", "title": "git push", "kind": "execute"},
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
            ],
        },
    ).get("outcome", {})
    with open("headless.txt", "w") as f:
        f.write("from the headless agent\n")
    committed = run(["git", "add", "headless.txt"])[0] and run(["git", "commit", "-qm", "headless"])[0]
    pushed, push_output = run(["git", "push", "origin", "main"])
    lines = [
        f"prompt: {text}",
        "mcp: " + ",".join(sorted(s.get("name", "?") for s in mcp_servers)),
        f"permission: {outcome.get('optionId', outcome.get('outcome'))}",
        "COMMIT_OK" if committed else "COMMIT_FAILED",
        "PUSH_REFUSED" if (not pushed and "gates.git.deny" in push_output) else "PUSH_NOT_REFUSED",
        "inside: " + ("container" if os.path.exists("/.dockerenv") else "host"),
    ]
    send({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": params["sessionId"],
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "\n".join(lines)}},
        },
    })
    return {"stopReason": "end_turn"}


for line in sys.stdin:
    message = json.loads(line)
    method, params, ident = message.get("method"), message.get("params", {}), message.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}
    elif method == "session/new":
        mcp_servers = params.get("mcpServers", [])
        result = {"sessionId": "fake-session"}
    elif method == "session/prompt":
        result = prompt(params)
    elif ident is None:
        continue
    else:
        send({"jsonrpc": "2.0", "id": ident, "error": {"code": -32601, "message": f"no method {method}"}})
        continue
    if ident is not None:
        send({"jsonrpc": "2.0", "id": ident, "result": result})
