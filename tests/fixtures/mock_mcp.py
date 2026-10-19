#!/usr/bin/env python3
"""Minimal MCP stdio server for tests: tools `add` and `shout`."""
import json
import sys

for line in sys.stdin:
    msg = json.loads(line)
    mid, method = msg.get("id"), msg.get("method")
    if mid is None:
        continue  # notification
    if method == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "mock", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [
            {"name": "add", "description": "soma", "inputSchema": {"type": "object", "properties": {
                "a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]}},
            {"name": "shout", "description": "grita", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        p = msg["params"]
        if p["name"] == "add":
            result = {"content": [{"type": "text", "text": str(p["arguments"]["a"] + p["arguments"]["b"])}]}
        else:
            result = {"content": [{"type": "text", "text": p["arguments"].get("text", "").upper()}], "isError": False}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}), flush=True)
