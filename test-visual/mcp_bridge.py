#!/usr/bin/env python3
"""MCP 持久桥接器 — 单 npx 子进程长连接，多请求复用，失败自动重连。
bash→stdin:  {"tool":"execute_js","args":{"code":"1+1"},"timeout":8}
stdout→bash: {"ok":true,"text":"2"} 或 {"ok":false,"error":"..."}
"""
import json, os, subprocess, sys, time, select, signal

def find_socket():
    p = os.environ.get("MCP_SOCK", "")
    if p and os.path.exists(p): return p
    for root, dirs, files in os.walk("/var/folders"):
        if "/0/" in root or "com.apple" in root:
            dirs.clear(); continue
        for f in files:
            if f == "tauri-mcp.sock": return os.path.join(root, f)
    return None

class Bridge:
    def __init__(self):
        self.proc = None
        self.reconnect()

    def reconnect(self):
        if self.proc:
            try: self.proc.kill(); self.proc.wait()
            except: pass
        sock = find_socket()
        if not sock: raise RuntimeError("socket not found")
        self.proc = subprocess.Popen(
            ["npx", "tauri-plugin-mcp-server"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env={**os.environ, "TAURI_MCP_IPC_PATH": sock}
        )
        time.sleep(4)

    def call(self, tool, args, timeout=8):
        req = json.dumps({
            "jsonrpc":"2.0","id":"vc","method":"tools/call",
            "params":{"name":tool,"arguments":args or {}}
        }) + "\n"
        try:
            self.proc.stdin.write(req.encode()); self.proc.stdin.flush()
        except BrokenPipeError:
            self.reconnect()
            self.proc.stdin.write(req.encode()); self.proc.stdin.flush()

        buf = b""
        deadline = time.time() + timeout
        while time.time() < deadline:
            r, _, _ = select.select([self.proc.stdout], [], [], 0.3)
            if r:
                try:
                    chunk = os.read(self.proc.stdout.fileno(), 65536)
                except (BlockingIOError, OSError):
                    chunk = b""
                if not chunk: break
                buf += chunk
                try:
                    d = json.loads(buf.decode())
                    if "result" in d or "error" in d: return d
                except json.JSONDecodeError: continue
        return {"error": "timeout"}

    def close(self):
        if self.proc:
            try: self.proc.kill(); self.proc.wait()
            except: pass

def main():
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
    bridge = None
    for line in sys.stdin:
        line = line.strip()
        if not line: continue
        try: req = json.loads(line)
        except: print(json.dumps({"ok":False,"error":"bad json"}), flush=True); continue
        tool = req.get("tool","")
        args = req.get("args",{})
        timeout = req.get("timeout",8)
        if bridge is None:
            try: bridge = Bridge()
            except Exception as e:
                print(json.dumps({"ok":False,"error":str(e)}), flush=True); continue
        try:
            resp = bridge.call(tool, args, timeout)
            if "error" in resp and "result" not in resp:
                bridge.reconnect()
                resp = bridge.call(tool, args, timeout)
            if "result" in resp:
                text = ""
                for it in resp["result"].get("content",[]):
                    if it.get("type") == "text":
                        text = it.get("text",""); break
                is_err = resp["result"].get("isError", False)
                print(json.dumps({"ok": not is_err, "text": text}), flush=True)
            else:
                print(json.dumps({"ok":False,"error":resp.get("error","unknown")}), flush=True)
        except Exception as e:
            print(json.dumps({"ok":False,"error":str(e)}), flush=True)
            try: bridge.reconnect()
            except: bridge = None

if __name__ == "__main__":
    main()
