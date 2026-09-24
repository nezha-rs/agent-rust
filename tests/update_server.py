"""Serve a local Rust update manifest and a test executable."""

import argparse
import hashlib
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--asset", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--bad-hash", action="store_true")
    args = parser.parse_args()
    payload = args.asset.read_bytes()
    digest = hashlib.sha256(payload).hexdigest()
    if args.bad_hash:
        digest = "0" * 64

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/manifest.json":
                body = json.dumps({
                    "version": "0.1.1",
                    "assets": {args.target: {
                        "url": "http://127.0.0.1:18542/asset",
                        "sha256": digest,
                    }},
                }).encode()
                content_type = "application/json"
            elif self.path == "/asset":
                body = payload
                content_type = "application/octet-stream"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    HTTPServer(("127.0.0.1", 18542), Handler).serve_forever()


if __name__ == "__main__":
    main()
