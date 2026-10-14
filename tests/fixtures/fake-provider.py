#!/usr/bin/env python3
"""A stand-in model provider for the gateway test: `fake-provider.py <port> <log>`.

Base URLs pick the behaviour: http://127.0.0.1:<port>/ok answers (JSON, or an SSE stream when the request asks for
one) with usage 10 in / 5 out; /limited answers 429. /v1/messages answers as Anthropic, /v1/chat/completions as
OpenAI. With tools in the request the answer also calls tool `lookup` with {"q": 1}, its arguments split over two
stream events. /v1/embeddings answers a vector of the input's letter counts (a-z, case ignored), so a reworded
prompt with the same letters is near. A model named *cached* reports prompt-cache tokens too: Anthropic 1000 read and
100 written beside the 10 input tokens, OpenAI 1000 read inside 1010 prompt tokens. Each call appends `<prefix> <model> <key>` to the log (the x-api-key or authorization header) and
`{"path", "body"}` to `<log>.bodies`.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port, log = int(sys.argv[1]), sys.argv[2]


def sse(events, named):
    return "".join(
        (f"event: {e['type']}\n" if named else "") + f"data: {e if isinstance(e, str) else json.dumps(e)}\n\n"
        for e in events
    ).encode()


def anthropic(body, text):
    tools = bool(body.get("tools"))
    cache = {"cache_read_input_tokens": 1000, "cache_creation_input_tokens": 100} if "cached" in body.get("model", "") else {}
    if not body.get("stream"):
        content = [{"type": "text", "text": text}]
        if tools:
            content.append({"type": "tool_use", "id": "tu_9", "name": "lookup", "input": {"q": 1}})
        return "application/json", json.dumps({
            "id": "msg_1", "type": "message", "role": "assistant", "model": body.get("model"), "content": content,
            "stop_reason": "tool_use" if tools else "end_turn", "stop_sequence": None,
            "usage": {"input_tokens": 10, "output_tokens": 5, **cache},
        }).encode()
    events = [
        {"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [],
                                              "model": body.get("model"), "usage": {"input_tokens": 10, "output_tokens": 1, **cache}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "ping"},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
        {"type": "content_block_stop", "index": 0},
    ]
    if tools:
        events += [
            {"type": "content_block_start", "index": 1,
             "content_block": {"type": "tool_use", "id": "tu_9", "name": "lookup", "input": {}}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": '{"q":'}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "1}"}},
            {"type": "content_block_stop", "index": 1},
        ]
    events += [
        {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tools else "end_turn", "stop_sequence": None},
         "usage": {"output_tokens": 5}},
        {"type": "message_stop"},
    ]
    return "text/event-stream", sse(events, True)


def embeddings(body, _text):
    letters = [0] * 26
    for c in body.get("input", "").lower():
        if "a" <= c <= "z":
            letters[ord(c) - ord("a")] += 1
    words = len(body.get("input", "").split())
    return "application/json", json.dumps({
        "data": [{"embedding": letters, "index": 0}], "model": body.get("model"),
        "usage": {"prompt_tokens": words, "total_tokens": words},
    }).encode()


def openai(body, text):
    tools = bool(body.get("tools"))
    base = {"id": "chatcmpl-1", "created": 1, "model": body.get("model")}
    usage = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    if "cached" in body.get("model", ""):
        usage = {"prompt_tokens": 1010, "completion_tokens": 5, "total_tokens": 1015,
                 "prompt_tokens_details": {"cached_tokens": 1000}}
    if not body.get("stream"):
        message = {"role": "assistant", "content": text}
        if tools:
            message["tool_calls"] = [
                {"id": "call_1", "type": "function", "function": {"name": "lookup", "arguments": '{"q":1}'}}
            ]
        choice = {"index": 0, "message": message, "finish_reason": "tool_calls" if tools else "stop"}
        return "application/json", json.dumps({**base, "object": "chat.completion", "choices": [choice],
                                               "usage": usage}).encode()

    def chunk(delta, finish=None):
        return {**base, "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}

    events: list[dict | str] = [chunk({"role": "assistant", "content": ""}), chunk({"content": text})]
    if tools:
        events += [
            chunk({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                                   "function": {"name": "lookup", "arguments": ""}}]}),
            chunk({"tool_calls": [{"index": 0, "function": {"arguments": '{"q":'}}]}),
            chunk({"tool_calls": [{"index": 0, "function": {"arguments": "1}"}}]}),
        ]
    events.append(chunk({}, "tool_calls" if tools else "stop"))
    if body.get("stream_options", {}).get("include_usage"):
        events.append({**base, "object": "chat.completion.chunk", "choices": [], "usage": usage})
    events.append("[DONE]")
    return "text/event-stream", sse(events, False)


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        prefix = self.path.split("/")[1]
        key = self.headers.get("x-api-key") or self.headers.get("authorization")
        with open(log, "a") as f:
            f.write(f"{prefix} {body.get('model')} {key}\n")
        with open(log + ".bodies", "a") as f:
            f.write(json.dumps({"path": self.path, "body": body}) + "\n")
        if prefix == "limited":
            self.send_response(429)
            self.send_header("content-type", "application/json")
            self.send_header("retry-after", "30")
            self.end_headers()
            self.wfile.write(b'{"type":"error","error":{"type":"rate_limit_error"}}')
            return
        text = f"model={body.get('model')} key={key}"
        api = (
            embeddings if self.path.endswith("/embeddings")
            else openai if self.path.endswith("/chat/completions")
            else anthropic
        )
        content_type, payload = api(body, text)
        self.send_response(200)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):  # noqa: A002 (the base class names it so)
        pass


ThreadingHTTPServer(("127.0.0.1", port), Provider).serve_forever()
