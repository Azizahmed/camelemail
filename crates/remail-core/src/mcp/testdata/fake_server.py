"""Minimal stdio MCP server used by the client tests.

tools/list is paginated (one tool per page). Calling `ask_ping` sends a
server->client ping and reports whether the client answered with {}.
`crash` exits immediately after writing to stderr.
"""
import json
import sys

TOOLS = [
    {"name": "echo", "description": "Echo text", "inputSchema": {"type": "object"}},
    {"name": "fail", "description": "Always fails", "inputSchema": {"type": "object"}},
    {"name": "ask_ping", "inputSchema": {"type": "object"}},
]


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def reply(req, result):
    send({"jsonrpc": "2.0", "id": req["id"], "result": result})


def main():
    print("fake server starting", file=sys.stderr, flush=True)
    sys.stdout.write("this is not json\n")
    while True:
        msg = read()
        method = msg.get("method")
        if "id" not in msg:
            continue
        if method == "initialize":
            reply(msg, {
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "0.0.1"},
                "instructions": "fake instructions",
            })
            send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "hi"}})
        elif method == "tools/list":
            index = int((msg.get("params") or {}).get("cursor") or 0)
            result = {"tools": [TOOLS[index]]}
            if index + 1 < len(TOOLS):
                result["nextCursor"] = str(index + 1)
            reply(msg, result)
        elif method == "tools/call":
            name = msg["params"]["name"]
            args = msg["params"].get("arguments") or {}
            if name == "echo":
                reply(msg, {"content": [{"type": "text", "text": args.get("text", "")},
                                        {"type": "image", "data": "", "mimeType": "image/png"}]})
            elif name == "fail":
                reply(msg, {"content": [{"type": "text", "text": "boom"}], "isError": True})
            elif name == "ask_ping":
                send({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"})
                send({"jsonrpc": "2.0", "id": "srv-2", "method": "sampling/createMessage", "params": {}})
                answers = {}
                while len(answers) < 2:
                    answer = read()
                    if answer.get("id") in ("srv-1", "srv-2"):
                        answers[answer["id"]] = answer
                ok = answers["srv-1"].get("result") == {} and answers["srv-2"]["error"]["code"] == -32601
                reply(msg, {"content": [{"type": "text", "text": "pong ok" if ok else "pong bad"}]})
            elif name == "crash":
                print("fatal: crashing on purpose", file=sys.stderr, flush=True)
                sys.exit(3)
            else:
                send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "unknown tool"}})
        elif method == "ping":
            reply(msg, {})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "nope"}})


if __name__ == "__main__":
    main()
