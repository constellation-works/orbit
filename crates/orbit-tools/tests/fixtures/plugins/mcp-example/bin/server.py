#!/usr/bin/env python3
"""Fixture stdio MCP server for the plugin `mcp` backend tests.

Speaks just enough of MCP (JSON-RPC 2.0, one message per line) for Orbit to
handshake, list tools, and call them. Tools:

- `echo`   — returns its arguments plus this process's pid, its working
             directory, `ORBIT_ALLOWED_TOOLS`, `ORBIT_WORKSPACE_ROOT` and the
             call's `params._meta`, so a test can prove one server serves many
             calls, that a narrower caller did not inherit a wider session, and
             that a second workspace got its own child rather than the first
             one's.
- `slow`   — sleeps `seconds` before answering, for timeout and concurrency
             tests.
- `crash`  — exits without answering, for dead-child tests.

`MCP_FIXTURE_EXTRA_TOOL=<name>` advertises one extra tool, and
`MCP_FIXTURE_ECHO_SCHEMA=<json>` replaces `echo`'s input schema, so a test can
make the server disagree with the manifest. `MCP_FIXTURE_SERVER_REQUEST=<method>`
makes `echo` send that server-initiated request and *wait* for the host's
answer before replying, which is how a client that drops server requests shows
up as a deadlock rather than as a quiet omission.
"""
import json
import os
import sys
import time

ECHO_SCHEMA = {
    "type": "object",
    "properties": {"message": {"type": "string"}},
}


def tools():
    echo_schema = ECHO_SCHEMA
    if os.environ.get("MCP_FIXTURE_ECHO_SCHEMA"):
        echo_schema = json.loads(os.environ["MCP_FIXTURE_ECHO_SCHEMA"])
    listed = [
        {"name": "echo", "description": "Echo the arguments.", "inputSchema": echo_schema},
        {
            "name": "slow",
            "description": "Answer after a delay.",
            "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}},
        },
        {"name": "crash", "description": "Exit mid-call.", "inputSchema": {"type": "object"}},
    ]
    extra = os.environ.get("MCP_FIXTURE_EXTRA_TOOL")
    if extra:
        listed.append({"name": extra, "inputSchema": {"type": "object"}})
    return listed


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def reply(request_id, result):
    send({"jsonrpc": "2.0", "id": request_id, "result": result})


def ask_host(method):
    """Send a server->client request and block until the host answers.

    The host is awaiting this server's `tools/call` reply, so the next line it
    writes is the answer to this request — or nothing at all, if it drops
    server requests, in which case this read is where the call dies.
    """
    send({"jsonrpc": "2.0", "id": "fixture-server-1", "method": method})
    line = sys.stdin.readline()
    return json.loads(line) if line.strip() else None


def call(request_id, params):
    name = params.get("name")
    arguments = params.get("arguments") or {}
    if name == "echo":
        server_request = os.environ.get("MCP_FIXTURE_SERVER_REQUEST")
        payload = {
            "echo": arguments,
            "pid": os.getpid(),
            "plugin": os.environ.get("ORBIT_PLUGIN"),
            "allowed": os.environ.get("ORBIT_ALLOWED_TOOLS", ""),
            "cwd": os.getcwd(),
            "workspace": os.environ.get("ORBIT_WORKSPACE_ROOT"),
            "meta": params.get("_meta"),
            "answer": ask_host(server_request) if server_request else None,
        }
        reply(request_id, {"content": [{"type": "text", "text": json.dumps(payload)}],
                           "structuredContent": payload})
    elif name == "slow":
        time.sleep(float(arguments.get("seconds", 5)))
        reply(request_id, {"content": [{"type": "text", "text": "done"}]})
    elif name == "crash":
        os._exit(3)
    else:
        reply(request_id, {"isError": True,
                           "content": [{"type": "text", "text": f"unknown tool {name}"}]})


while True:
    # `readline` rather than iterating stdin: `ask_host` reads one line of its
    # own mid-call, and the iterator's read-ahead buffer would swallow it.
    line = sys.stdin.readline()
    if not line:
        break
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        reply(request_id, {"protocolVersion": message["params"].get("protocolVersion", "2025-06-18"),
                           "capabilities": {"tools": {}},
                           "serverInfo": {"name": "mcp-example", "version": "0.1.0"}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        reply(request_id, {"tools": tools()})
    elif method == "tools/call":
        call(request_id, message.get("params") or {})
    elif request_id is not None:
        send({"jsonrpc": "2.0", "id": request_id,
              "error": {"code": -32601, "message": f"unknown method {method}"}})
