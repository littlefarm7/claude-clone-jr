#!/usr/bin/env python3
"""Deterministic OpenAI-compatible server used by end-to-end tests.

It behaves like a tool-calling model following a fixed strategy, so the
real `jarvis` binary, HTTP client, permission system and tools are exercised
without network access or model weights. Usage: mock_llm.py PORT
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

CALLS = 0


def call(name, args):
    global CALLS
    CALLS += 1
    return {"role": "assistant", "content": None, "tool_calls": [
        {"id": f"call_{CALLS}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}


def text(t):
    return {"role": "assistant", "content": t}


def decide(messages):
    user_task = next(m["content"] for m in messages if m["role"] == "user")
    tool_msgs = [m for m in messages if m["role"] == "tool"]
    last = messages[-1]
    verifying = any(m["role"] == "user" and "VERIFICAÇÃO" in (m["content"] or "") for m in messages)
    if verifying:
        if last["role"] == "tool":
            ok = "relatorio" in last["content"] and "linhas" in last["content"]
            return text("O arquivo existe e tem o conteúdo esperado.\nVERDICT: PASS" if ok else "VERDICT: FAIL: conteúdo inesperado")
        return call("read_file", {"path": "saida/relatorio.txt"})
    n = len(tool_msgs)
    if "relatório" in user_task:
        steps = [
            lambda: call("update_plan", {"steps": [
                {"title": "contar linhas de dados.csv", "status": "in_progress"},
                {"title": "escrever relatório", "status": "pending"}]}),
            lambda: call("shell", {"command": "wc -l < dados.csv"}),
            lambda: call("write_file", {"path": "saida/relatorio.txt",
                                        "content": "relatorio: dados.csv tem %s linhas\n" % tool_msgs[-1]["content"].split("--- stdout ---\n")[1].strip()}),
            lambda: call("update_plan", {"steps": [
                {"title": "contar linhas de dados.csv", "status": "completed"},
                {"title": "escrever relatório", "status": "completed"}]}),
            lambda: text("Relatório criado em saida/relatorio.txt."),
        ]
        return steps[min(n, len(steps) - 1)]()
    if "sudo" in user_task:
        if n == 0:
            return call("shell", {"command": "sudo apt install cowsay"})
        return text("Não foi possível: " + tool_msgs[-1]["content"][:80])
    return text("Olá! Sou o mock.")


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        self.send_response(200 if self.path in ("/health", "/v1/models") else 404)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        msg = decide(body["messages"])
        resp = {"choices": [{"index": 0, "message": msg,
                             "finish_reason": "tool_calls" if msg.get("tool_calls") else "stop"}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 20}}
        data = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
