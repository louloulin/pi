#!/usr/bin/env python3
"""Minimal OpenAI-compatible SSE server for PTY evidence.

Why this exists
---------------
`faux/faux-model` only ever replies with text, so a PTY scenario cannot
make the real TUI run a *tool*. Tool-hook evidence (LUM-1330) needs the
agent loop to actually dispatch a tool call, and the cheapest honest way
to get one is to point the TUI at a local server that speaks the
OpenAI chat-completions protocol and scripts the reply.

The server answers the first request with a tool call and every later
request with plain text, so the transcript ends with the tool result the
agent loop produced.

Usage
-----
    python3 pi-rust/scripts/fake_model_server.py --port 8137 \
        --tool bash --arguments '{"command": "echo forbidden"}'

Stop it with Ctrl-C; it serves until then. The matching scenario sets
`OPENAI_BASE_URL=http://127.0.0.1:<port>` and `OPENAI_API_KEY=test-key`,
so no real credential and no network egress are involved.
"""

from __future__ import annotations

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def chunk(delta: dict, finish_reason: str | None) -> str:
    payload = {
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": "gpt-4o-mini",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    }
    return f"data: {json.dumps(payload)}\n\n"


def usage_chunk() -> str:
    payload = {
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": "gpt-4o-mini",
        "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
    }
    return f"data: {json.dumps(payload)}\n\n"


def tool_call_body(tool: str, arguments: dict) -> str:
    encoded = json.dumps(arguments)
    body = chunk({"role": "assistant", "content": None}, None)
    body += chunk(
        {
            "tool_calls": [
                {
                    "index": 0,
                    "id": "call_fake",
                    "type": "function",
                    "function": {"name": tool, "arguments": ""},
                }
            ]
        },
        None,
    )
    body += chunk(
        {"tool_calls": [{"index": 0, "function": {"arguments": encoded}}]},
        None,
    )
    body += chunk({}, "tool_calls")
    body += usage_chunk()
    return body + "data: [DONE]\n\n"


def text_body(text: str) -> str:
    body = chunk({"role": "assistant", "content": ""}, None)
    body += chunk({"content": text}, None)
    body += chunk({}, "stop")
    body += usage_chunk()
    return body + "data: [DONE]\n\n"


class Handler(BaseHTTPRequestHandler):
    """First request: the scripted tool call. Later requests: plain text."""

    tool = "bash"
    arguments: dict = {}
    text = "finished"
    requests = 0

    def do_POST(self) -> None:  # noqa: N802 — http.server API
        length = int(self.headers.get("content-length") or 0)
        self.rfile.read(length)  # the request body is not needed
        if Handler.requests == 0:
            body = tool_call_body(Handler.tool, Handler.arguments)
        else:
            body = text_body(Handler.text)
        Handler.requests += 1
        payload = body.encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args) -> None:
        """Silence the per-request access log."""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8137)
    parser.add_argument("--tool", default="bash")
    parser.add_argument("--arguments", default='{"command": "echo forbidden"}')
    parser.add_argument("--text", default="finished")
    args = parser.parse_args()

    Handler.tool = args.tool
    Handler.arguments = json.loads(args.arguments)
    Handler.text = args.text
    Handler.requests = 0

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"fake model server on http://127.0.0.1:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
