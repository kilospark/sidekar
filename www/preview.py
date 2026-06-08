#!/usr/bin/env python3
"""Serve www/public locally with vercel.json static page rewrites."""

from __future__ import annotations

import json
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

WWW = Path(__file__).resolve().parent
PUBLIC = WWW / "public"
VERCEL = WWW / "vercel.json"


def static_rewrites() -> dict[str, str]:
    data = json.loads(VERCEL.read_text())
    out: dict[str, str] = {}
    for rule in data.get("rewrites", []):
        src = rule.get("source", "")
        dst = rule.get("destination", "")
        if not src.startswith("/") or ":path" in src or ":id" in src:
            continue
        if ":version" in src or ":asset" in src:
            continue
        if dst.startswith("/api/"):
            continue
        out[src] = dst
    return out


REWRITES = static_rewrites()


class PreviewHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(PUBLIC), **kwargs)

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        target = REWRITES.get(path)
        if target is not None:
            rest = self.path[len(path) :]
            self.path = target + rest
        super().do_GET()


def main() -> None:
    port = 8080
    if len(sys.argv) > 1:
        port = int(sys.argv[1])
    server = ThreadingHTTPServer(("127.0.0.1", port), PreviewHandler)
    print(f"Sidekar site preview: http://127.0.0.1:{port}/")
    print(f"Docs: http://127.0.0.1:{port}/docs")
    print("Press Ctrl-C to stop.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print()
        server.server_close()


if __name__ == "__main__":
    main()
