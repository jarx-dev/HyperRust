#!/usr/bin/env python3
"""HyperRust dictation history server: receives entries from the app over
Tailscale and keeps them in ~/hyperrust-history/<device>.jsonl (newest first
out, at most HYPERRUST_HISTORY_KEEP per device). Write-only on purpose: the
history is read locally on this machine (jq, `prompt_eval --history`), never
served over the network. Files are private to the user (0600).

    POST /history   JSON array of entries (or one entry); each needs a string "id".
                    Ids already stored are skipped, so clients may resend.
"""

import json
import os
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DIR = os.path.expanduser(os.environ.get("HYPERRUST_HISTORY_DIR", "~/hyperrust-history"))
BIND = os.environ.get("HYPERRUST_HISTORY_BIND", "127.0.0.1")
PORT = int(os.environ.get("HYPERRUST_HISTORY_PORT", "8791"))
KEEP = int(os.environ.get("HYPERRUST_HISTORY_KEEP", "2000"))
MAX_BODY = 1 << 20
MAX_ENTRIES = 100
LOCK = threading.Lock()


def device_file(device):
    name = re.sub(r"[^A-Za-z0-9._-]", "_", str(device or ""))[:64].lstrip(".")
    return os.path.join(DIR, (name or "nieznany") + ".jsonl")


def ts_of(e):
    ts = e.get("ts_ms")
    return ts if isinstance(ts, int) else 0


def load(path):
    """Existing entries as (ts_ms, id, line)."""
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except FileNotFoundError:
        return []
    rows = []
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict):
            rows.append((ts_of(e), e.get("id"), line))
    return rows


def store(entries):
    by_file = {}
    for e in entries:
        by_file.setdefault(device_file(e.get("device")), []).append(e)
    added = 0
    with LOCK:
        for path, items in by_file.items():
            rows = load(path)
            seen = {r[1] for r in rows}
            new = []
            for e in items:
                if e["id"] in seen:
                    continue
                seen.add(e["id"])
                new.append((ts_of(e), e["id"], json.dumps(e, ensure_ascii=False)))
            if not new:
                continue
            added += len(new)
            rows = sorted(rows + new, key=lambda r: r[0])[-KEEP:]
            tmp = path + ".tmp"
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write("".join(r[2] + "\n" for r in rows))
            os.replace(tmp, path)
    return added


class Handler(BaseHTTPRequestHandler):
    server_version = "hyperrust-history"

    def reply(self, code, body):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        if self.path != "/history":
            return self.reply(404, {"error": "nieznana ścieżka"})
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError:
            return self.reply(411, {"error": "brak Content-Length"})
        if not 0 < length <= MAX_BODY:
            return self.reply(413, {"error": "za duże zapytanie"})
        try:
            data = json.loads(self.rfile.read(length))
        except ValueError:
            return self.reply(400, {"error": "niepoprawny JSON"})
        if isinstance(data, dict):
            data = [data]
        if not isinstance(data, list) or len(data) > MAX_ENTRIES:
            return self.reply(400, {"error": f"oczekiwano tablicy do {MAX_ENTRIES} wpisów"})
        entries = [e for e in data if isinstance(e, dict) and isinstance(e.get("id"), str) and 0 < len(e["id"]) <= 128]
        try:
            added = store(entries)
        except OSError as e:
            return self.reply(500, {"error": f"zapis: {e.strerror}"})
        self.reply(200, {"added": added, "received": len(entries)})


def main():
    os.makedirs(DIR, mode=0o700, exist_ok=True)
    ThreadingHTTPServer((BIND, PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
