#!/usr/bin/env python3
"""Fixture stdio MCP server for the plugin `mcp` backend tests.

Speaks just enough of MCP (JSON-RPC 2.0, one message per line) for Orbit to
handshake, list tools, and call them. Tools:

- `echo`   — returns its arguments plus this process's pid, so a test can
             prove one server serves many calls.
- `slow`   — sleeps `seconds` before answering, for timeout tests.
- `crash`  — exits without answering, for dead-child tests.

`MCP_FIXTURE_EXTRA_TOOL=<name>` advertises one extra tool, and
`MCP_FIXTURE_ECHO_SCHEMA=<json>` replaces `echo`'s input schema, so a test can
make the server disagree with the manifest.
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


def reply(request_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
    sys.stdout.flush()


def call(request_id, params):
    name = params.get("name")
    arguments = params.get("arguments") or {}
    if name == "echo":
        payload = {"echo": arguments, "pid": os.getpid(), "plugin": os.environ.get("ORBIT_PLUGIN")}
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


for line in sys.stdin:
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
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id,
                                     "error": {"code": -32601, "message": f"unknown method {method}"}}) + "\n")
        sys.stdout.flush()
