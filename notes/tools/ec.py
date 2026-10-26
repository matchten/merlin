"""Tiny EffectCraft control-channel client: ec.py METHOD [JSON_PARAMS]"""
import json, socket, sys

def call(method, params=None, port=9877):
    with socket.create_connection(("127.0.0.1", port), timeout=60) as s:
        s.sendall((json.dumps({"id": 1, "method": method, "params": params or {}}) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    return json.loads(buf)

if __name__ == "__main__":
    params = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
    print(json.dumps(call(sys.argv[1], params), indent=1)[:4000])
