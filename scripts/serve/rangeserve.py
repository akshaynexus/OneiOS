#!/usr/bin/env python3
"""Serve the files of one folder over HTTP with byte ranges, so a download manager can fetch one file in many
parallel parts (and from several mirrors of the same server at once).

    rangeserve.py DIR PORT        binds 127.0.0.1 only: reach it through a tunnel, never from the LAN

Only plain files directly in DIR (or symlinks to files) are served; no listing beyond them, no paths with '/'.
"""
import http.server
import os
import sys
import urllib.parse

ROOT = os.path.realpath(sys.argv[1])
PORT = int(sys.argv[2])
CHUNK = 1 << 20


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _file(self):
        name = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path).lstrip("/")
        if not name or "/" in name or name.startswith("."):
            return None
        path = os.path.join(ROOT, name)
        return path if os.path.isfile(path) else None

    def _range(self, size):
        h = self.headers.get("Range")
        if not h or not h.startswith("bytes=") or "," in h:
            return None
        a, _, b = h[6:].strip().partition("-")
        if a == "":
            n = int(b)
            return (max(0, size - n), size - 1) if n > 0 else "bad"
        start = int(a)
        end = int(b) if b else size - 1
        if start >= size or end < start:
            return "bad"
        return start, min(end, size - 1)

    def _head(self, send_body):
        path = self._file()
        if path is None:
            self.send_error(404)
            return
        size = os.path.getsize(path)
        try:
            rng = self._range(size)
        except ValueError:
            rng = "bad"
        if rng == "bad":
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        start, end = rng if rng else (0, size - 1)
        self.send_response(206 if rng else 200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        if rng:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        if not send_body:
            return
        with open(path, "rb") as f:
            f.seek(start)
            left = end - start + 1
            while left > 0:
                buf = f.read(min(CHUNK, left))
                if not buf:
                    break
                try:
                    self.wfile.write(buf)
                except (BrokenPipeError, ConnectionResetError):
                    return
                left -= len(buf)

    def do_GET(self):
        self._head(True)

    def do_HEAD(self):
        self._head(False)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.log_date_time_string(), fmt % args))


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
