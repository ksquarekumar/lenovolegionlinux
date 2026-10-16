#!/usr/bin/env python3
"""A stand-in ACP agent (protocol v1, newline-delimited JSON-RPC on stdio) for the headless-run tests.

On a prompt it announces a tool call, asks one permission, commits a file and tries a push in its working folder
(inside the session's sandbox), then answers with what it saw: the prompt, its ACP session (new, or loaded), the MCP
servers from session/new, the permission outcome, and whether the commit and the push went through. The answer comes
line by line, 0.1 s apart; a prompt that starts with WAIT holds after the first line until a file `release` is in the
working folder, or the file $FAKE_ACP_RELEASE when that is set (at most 30 s), and one that starts with SLOW waits 30 s first (a long run, for the test that stops the
operator in the middle of one). It can load its sessions (kept in `.fake-acp-sessions` in the working folder, so a
new adapter process finds them): session/load replays the earlier prompts and a REPLAYED line before it answers.
After session/new and session/load it offers two commands (available_commands_update): /plan <what to plan> and
/status. In a thread member's session (LEGIONCTL_MEMBER=<thread>/<member>), a prompt runs the script
$FAKE_ACP_SCRIPTS/<member>.sh instead, with the prompt in FAKE_ACP_PROMPT, and its output lines are the answer. A name
as its first argument ends each answer with the line "agent: <name>" (two session.acp agents in one test). Like the real adapters, it does not end when its input closes: the operator must end the session.
A ui-composer (DESIGN-09 G5): session/set_model keeps the model, and the answer has the line "model: <model>"; with
FAKE_ACP_VIEWS set to a folder, a prompt with a line "Intent: <words>" gets the file <first word>.jsonc of that folder
as a ```json block at the end of the answer (no file: no block).
"""

import json
import os
import subprocess
import sys
import threading
import time
import uuid

SESSIONS = ".fake-acp-sessions"
model = None
mcp_servers = []
loaded = None
next_id = 100


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def update(session, what, **fields):
    send({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": session, "update": {"sessionUpdate": what, **fields}},
    })


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


def sessions():
    try:
        with open(SESSIONS) as f:
            return json.load(f)
    except OSError:
        return {}


def keep(session, prompts):
    known = sessions()
    known[session] = prompts
    with open(SESSIONS, "w") as f:
        json.dump(known, f)


def where():
    if os.path.exists("/.dockerenv"):
        return "container"
    try:
        with open("/sys/class/dmi/id/sys_vendor") as f:
            return "vm" if "QEMU" in f.read() else "host"
    except OSError:
        return "host"


def run(argv):
    done = subprocess.run(argv, capture_output=True, text=True)
    return done.returncode == 0, done.stdout + done.stderr


def prompt(params):
    session = params["sessionId"]
    text = " ".join(block.get("text", "") for block in params.get("prompt", []))
    keep(session, sessions().get(session, []) + [text])
    script = os.path.join(os.environ.get("FAKE_ACP_SCRIPTS", ""), os.environ.get("LEGIONCTL_MEMBER", "").split("/")[-1] + ".sh")
    if os.environ.get("LEGIONCTL_MEMBER") and os.path.isfile(script):
        return member(session, text, script)
    if text.startswith("SLOW"):
        time.sleep(30)
    update(session, "tool_call", toolCallId="t1", title="git push", kind="execute", status="pending")
    outcome = request(
        "session/request_permission",
        {
            "sessionId": session,
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
        f"acp: {'loaded' if loaded == session else 'new'} {session}",
        "mcp: " + ",".join(sorted(s.get("name", "?") for s in mcp_servers)),
        f"permission: {outcome.get('optionId', outcome.get('outcome'))}",
        "COMMIT_OK" if committed else "COMMIT_FAILED",
        "PUSH_REFUSED" if (not pushed and "gates.git.deny" in push_output) else "PUSH_NOT_REFUSED",
        "inside: " + where(),
    ]
    if len(sys.argv) > 1:
        lines.append(f"agent: {sys.argv[1]}")
    if model:
        lines.append(f"model: {model}")
    views = os.environ.get("FAKE_ACP_VIEWS")
    intent = next((l[len("Intent: "):] for l in text.splitlines() if l.startswith("Intent: ")), "")
    spec = os.path.join(views or "", (intent.split() or [""])[0] + ".jsonc")
    if views and intent and os.path.isfile(spec):
        lines.append("Here is the view.")
        lines.append("```json\n" + open(spec).read().strip() + "\n```")
    for i, line in enumerate(lines):
        update(session, "agent_message_chunk", content={"type": "text", "text": line + "\n"})
        if i == 0 and text.startswith("WAIT"):
            deadline = time.time() + 30
            release = os.environ.get("FAKE_ACP_RELEASE", "release")
            while not os.path.exists(release) and time.time() < deadline:
                time.sleep(0.1)
        time.sleep(0.1)
    return {"stopReason": "end_turn"}


def member(session, text, script):
    """A thread member's wake: its archetype's script runs with the wake prompt, and each line it prints is a chunk of
    the answer as it comes (a long or a loud script runs into the wake's budget)."""
    loaded_note = "loaded" if loaded == session else "new"
    update(session, "agent_message_chunk", content={"type": "text", "text": f"acp: {loaded_note} {session}\n"})
    child = subprocess.Popen(["sh", script], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                             env={**os.environ, "FAKE_ACP_PROMPT": text})
    for out in child.stdout:
        update(session, "agent_message_chunk", content={"type": "text", "text": out})
    child.wait()
    return {"stopReason": "end_turn"}


for line in sys.stdin:
    message = json.loads(line)
    method, params, ident = message.get("method"), message.get("params", {}), message.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}, "authMethods": []}
    elif method == "session/new":
        mcp_servers = params.get("mcpServers", [])
        result = {"sessionId": f"fake-{uuid.uuid4().hex[:12]}"}
        keep(result["sessionId"], [])
    elif method == "session/load" and params.get("sessionId") in sessions():
        mcp_servers, loaded = params.get("mcpServers", []), params["sessionId"]
        for earlier in sessions()[loaded]:
            update(loaded, "user_message_chunk", content={"type": "text", "text": earlier})
            update(loaded, "agent_message_chunk", content={"type": "text", "text": "REPLAYED\n"})
        result = {}
    elif method == "session/prompt":
        result = prompt(params)
    elif method == "session/set_model":
        model = params.get("modelId")
        result = {}
    elif ident is None:
        continue
    else:
        send({"jsonrpc": "2.0", "id": ident, "error": {"code": -32601, "message": f"no method {method}"}})
        continue
    if ident is not None:
        send({"jsonrpc": "2.0", "id": ident, "result": result})
    if method in ("session/new", "session/load"):
        # as the real adapters: the session's commands come after session/new or session/load
        update(result.get("sessionId", loaded), "available_commands_update", availableCommands=[
            {"name": "plan", "description": "Plan a change", "input": {"hint": "what to plan"}},
            {"name": "status", "description": "Show the session state"},
        ])

threading.Event().wait()
