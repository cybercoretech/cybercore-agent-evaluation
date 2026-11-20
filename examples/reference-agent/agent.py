"""A small reference agent for the CyberCore Agent Evaluation Workbench.

It shows how to run a real model-backed agent under the workbench
(ADR-0025):

- the task arrives as the TASK_PROMPT environment variable plus input files
  in /workspace/input;
- the agent works with three file tools, confined to /workspace/input
  (read only) and /workspace/output (read and write);
- it calls the Claude API through the official Anthropic SDK. Inside the
  container the only way out is the workbench's egress proxy, which the SDK
  reaches through HTTPS_PROXY;
- whatever happens, it writes /workspace/output/cybercore-usage.json from
  the API's own usage counts, so the workbench can report tokens and cost.

Configuration (environment):
  TASK_PROMPT        the task (required)
  ANTHROPIC_API_KEY  granted by the workbench (runner.egress.credentials)
  AGENT_MODEL        model ID (default claude-opus-5-5)
  AGENT_MAX_TURNS    most model calls per run (default 12)
  ANTHROPIC_BASE_URL optional API base URL (the fake API in tests)
  AGENT_CA_FILE      optional extra CA certificate to trust (the fake API)
"""

import json
import os
import ssl
import sys
from pathlib import Path

import anthropic
from anthropic import beta_tool

INPUT = Path("/workspace/input")
OUTPUT = Path("/workspace/output")
USAGE_FILE = OUTPUT / "cybercore-usage.json"
MAX_READ_BYTES = 256 * 1024
MAX_WRITE_BYTES = 1024 * 1024

SYSTEM = (
    "You are an agent being evaluated. Complete the task using the file tools. "
    "Input files are in /workspace/input (read only); write every result under "
    "/workspace/output. When the task is done, reply with a one-line summary."
)


def confined(path: str, roots: tuple[Path, ...]) -> Path:
    """Resolves a path and refuses anything outside the allowed roots."""
    resolved = Path(path).resolve()
    for root in roots:
        if resolved == root or root in resolved.parents:
            return resolved
    raise ValueError(f"{path} is outside {', '.join(str(r) for r in roots)}")


class Usage:
    """Token counts summed over every API response, per model."""

    def __init__(self) -> None:
        self.models: dict[str, list[int]] = {}
        self.tool_calls = 0

    def add(self, message) -> None:
        usage = message.usage
        # Cache reads and writes are input tokens too; the workbench prices
        # all input at one rate, so this slightly overstates cached runs.
        input_tokens = (
            usage.input_tokens
            + (getattr(usage, "cache_creation_input_tokens", 0) or 0)
            + (getattr(usage, "cache_read_input_tokens", 0) or 0)
        )
        counts = self.models.setdefault(message.model, [0, 0])
        counts[0] += input_tokens
        counts[1] += usage.output_tokens
        self.tool_calls += sum(1 for block in message.content if block.type == "tool_use")

    def write(self) -> None:
        if not self.models:
            return
        report = {
            "format": "cybercore-usage/v0",
            "models": [
                {"model_id": model, "input_tokens": i, "output_tokens": o}
                for model, (i, o) in sorted(self.models.items())
            ],
            "tool_calls": self.tool_calls,
        }
        OUTPUT.mkdir(parents=True, exist_ok=True)
        USAGE_FILE.write_text(json.dumps(report))


@beta_tool
def list_files(directory: str) -> str:
    """List the files under a directory.

    Args:
        directory: /workspace/input or /workspace/output, or a directory inside them.
    """
    root = confined(directory, (INPUT, OUTPUT))
    if not root.is_dir():
        return f"{directory} is not a directory"
    names = sorted(str(p) for p in root.rglob("*") if p.is_file() and p != USAGE_FILE)
    return "\n".join(names) or "(empty)"


@beta_tool
def read_file(path: str) -> str:
    """Read a text file.

    Args:
        path: An absolute path under /workspace/input or /workspace/output.
    """
    file = confined(path, (INPUT, OUTPUT))
    data = file.read_bytes()[:MAX_READ_BYTES]
    return data.decode("utf-8", errors="replace")


@beta_tool
def write_file(path: str, content: str) -> str:
    """Write a text file, creating parent directories.

    Args:
        path: An absolute path under /workspace/output.
        content: The full file content.
    """
    file = confined(path, (OUTPUT,))
    if file == USAGE_FILE:
        raise ValueError("that file is reserved for the usage report")
    data = content.encode("utf-8")
    if len(data) > MAX_WRITE_BYTES:
        raise ValueError("content is larger than 1 MiB")
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_bytes(data)
    return f"wrote {len(data)} bytes to {path}"


def client() -> anthropic.Anthropic:
    ca_file = os.environ.get("AGENT_CA_FILE")
    if not ca_file:
        return anthropic.Anthropic()
    context = ssl.create_default_context()
    context.load_verify_locations(cafile=ca_file)
    return anthropic.Anthropic(http_client=anthropic.DefaultHttpxClient(verify=context))


def main() -> int:
    prompt = os.environ.get("TASK_PROMPT", "").strip()
    if not prompt:
        print("TASK_PROMPT is not set", file=sys.stderr)
        return 2
    usage = Usage()
    final = ""
    try:
        runner = client().beta.messages.tool_runner(
            model=os.environ.get("AGENT_MODEL", "claude-opus-5-5"),
            max_tokens=16000,
            thinking={"type": "adaptive"},
            system=SYSTEM,
            tools=[list_files, read_file, write_file],
            messages=[{"role": "user", "content": prompt}],
            max_iterations=int(os.environ.get("AGENT_MAX_TURNS", "12")),
        )
        message = None
        for message in runner:
            usage.add(message)
        if message is not None:
            final = "".join(b.text for b in message.content if b.type == "text").strip()
            if message.stop_reason not in ("end_turn", "stop_sequence"):
                print(f"stopped: {message.stop_reason}", file=sys.stderr)
    except anthropic.APIError as error:
        # The status and type only: never echo request details or keys.
        status = getattr(error, "status_code", None)
        print(f"API error: {type(error).__name__} {status or ''}".strip(), file=sys.stderr)
        return 1
    finally:
        usage.write()
    print(final)
    return 0


if __name__ == "__main__":
    sys.exit(main())
