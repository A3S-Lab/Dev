#!/usr/bin/env python3
import os, http.server, datetime

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"pid={os.getpid()} time={datetime.datetime.now().isoformat()}\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        print(f"{self.address_string()} - {format % args}", flush=True)

port = int(os.environ.get("PORT", 8000))
http.server.HTTPServer(("", port), Handler).serve_forever()
