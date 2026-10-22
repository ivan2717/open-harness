"""Dependency-free test MCP peer, launched directly without a shell."""
import json
import os
import sys
import time

mode = sys.argv[1]
if len(sys.argv) > 2:
    with open(sys.argv[2], "w") as pid:
        pid.write(str(os.getpid()))

initialized = False
calls = 0
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "notifications/initialized":
        initialized = True
        continue
    if "id" not in request:
        continue
    params = request.get("params", {})
    if method == "initialize":
        if mode == "hang_initialize":
            time.sleep(30)
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}, "prompts": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
    elif not initialized:
        raise RuntimeError("request arrived before initialized notification")
    elif method == "tools/list":
        cursor = params.get("cursor")
        name = "second" if cursor else "first"
        result = {"tools": [{"name": name, "description": "fixture tool", "inputSchema": {"type": "object"}}]}
        if not cursor or mode == "repeat_cursor":
            result["nextCursor"] = "next"
    elif method == "resources/list":
        result = {"resources": [{"uri": "fixture:///document", "name": "document"}]}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": []}
    elif method == "prompts/list":
        result = {"prompts": [{"name": "review", "description": "Review a document"}]}
    elif method == "resources/read":
        result = {"contents": [{"uri": params["uri"], "text": "resource body", "mimeType": "text/plain"}]}
    elif method == "prompts/get":
        result = {"messages": [{"role": "user", "content": {"type": "text", "text": "review it"}}]}
    elif method == "tools/call":
        calls += 1
        if params["name"] == "hang":
            time.sleep(30)
        if params["name"] == "rpc_error":
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32602, "message": "secret-token"}}), flush=True)
            continue
        text = "x" * 5000000 if params["name"] == "oversize" else "tool result"
        result = {"content": [{"type": "text", "text": text}], "structuredContent": {"calls": calls, "arguments": params.get("arguments"), "environment": sorted(os.environ.keys())}, "isError": params["name"] == "tool_error"}
    else:
        raise RuntimeError("unexpected method " + method)
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
