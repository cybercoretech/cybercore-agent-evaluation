"""A fake Claude Messages API for testing the reference agent without cost.

It answers POST /v1/messages over HTTPS like the real API does for the
reference agent's file tools, including message threads (the SDK's tool
runner sends only new messages and continues the stored conversation),
following a fixed script that solves the
example "sum the numbers" tasks:

  1. list /workspace/input      (tool_use list_files)
  2. read the first .txt file   (tool_use read_file)
  3. write the total            (tool_use write_file /workspace/output/out/report.txt)
  4. finish                     (end_turn)

A model ID containing "haiku" plays a weaker model: it writes the number of
lines instead of the total, so suites show a real difference. Every response
reports fixed usage (1000 input and 100 output tokens per call, doubled for
non-haiku models), so tests can check token and cost figures exactly.

Requests without the expected key (x-api-key or bearer token) get a 401, which proves the
workbench granted the credential. Standard library only.

  python3 fake_api.py --cert cert.pem --key key.pem --expect-key KEY [--port 0]

It prints "listening on PORT" once ready.
"""

import argparse
import json
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from itertools import count

IDS = count(1)
# Message threads (the SDK's tool runner uses them): the conversation stored
# under each response ID, so a continue request can send only new messages.
THREADS: dict[str, list] = {}


def text_of(content) -> str:
    if isinstance(content, str):
        return content
    return "".join(block.get("text", "") for block in content if isinstance(block, dict))


def last_tool_exchange(messages):
    """(tool name, tool input, result text) of the most recent tool call."""
    if len(messages) < 2 or messages[-1]["role"] != "user":
        return None
    results = [b for b in messages[-1]["content"] if isinstance(b, dict) and b.get("type") == "tool_result"]
    if not results:
        return None
    calls = [b for b in messages[-2]["content"] if isinstance(b, dict) and b.get("type") == "tool_use"]
    if not calls:
        return None
    result = results[0]
    return calls[0]["name"], calls[0].get("input", {}), text_of(result.get("content", ""))


def next_step(model: str, messages):
    """The scripted assistant turn: ("tool", name, input) or ("text", reply)."""
    exchange = last_tool_exchange(messages)
    if exchange is None:
        return ("tool", "list_files", {"directory": "/workspace/input"})
    name, _, result = exchange
    if name == "list_files":
        files = [line for line in result.splitlines() if line.endswith(".txt")]
        if not files:
            return ("text", "no input file to read")
        return ("tool", "read_file", {"path": files[0]})
    if name == "read_file":
        lines = [line for line in result.splitlines() if line.strip()]
        if "haiku" in model:
            answer = len(lines)
        else:
            answer = sum(int(line) for line in lines)
        return ("tool", "write_file", {"path": "/workspace/output/out/report.txt", "content": f"{answer}\n"})
    return ("text", "done: wrote the total to /workspace/output/out/report.txt")


class Handler(BaseHTTPRequestHandler):
    expected_key = ""

    def log_message(self, format, *args):  # quiet; the test reads stdout
        pass

    def reply(self, status: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        request = json.loads(self.rfile.read(length) or b"{}")
        if self.path.split("?")[0] not in ("/v1/messages",):
            return self.reply(404, {"type": "error", "error": {"type": "not_found_error", "message": self.path}})
        # The SDK sends the key as x-api-key or as a bearer token.
        presented = {self.headers.get("x-api-key"), self.headers.get("authorization", "").removeprefix("Bearer ")}
        if self.expected_key not in presented:
            # Names and lengths only: never print a key.
            seen = {k: len(v) for k, v in self.headers.items() if k.lower() in ("x-api-key", "authorization")}
            print(f"401: auth headers {seen}", file=sys.stderr, flush=True)
            return self.reply(401, {"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}})
        model = request.get("model", "unknown")
        thread = request.get("thread") or {}
        messages = request.get("messages", [])
        if thread.get("type") == "continue":
            earlier = THREADS.pop(thread.get("previous_message_id", ""), None)
            if earlier is None:
                return self.reply(404, {"type": "error", "error": {
                    "type": "not_found_error", "message": "thread not found",
                    "details": {"error_code": "thread_not_found"}}})
            messages = earlier + messages
        step = next_step(model, messages)
        n = next(IDS)
        if step[0] == "tool":
            content = [{"type": "tool_use", "id": f"toolu_fake_{n:04d}", "name": step[1], "input": step[2]}]
            stop = "tool_use"
        else:
            content = [{"type": "text", "text": step[1]}]
            stop = "end_turn"
        scale = 1 if "haiku" in model else 2
        if thread.get("type") in ("create", "continue"):
            THREADS[f"msg_fake_{n:04d}"] = messages + [{"role": "assistant", "content": content}]
        self.reply(200, {
            "id": f"msg_fake_{n:04d}",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content,
            "stop_reason": stop,
            "stop_sequence": None,
            "usage": {
                "input_tokens": 1000 * scale,
                "output_tokens": 100 * scale,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
        })


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--expect-key", required=True)
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    Handler.expected_key = args.expect_key
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print(f"listening on {server.server_address[1]}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
