#!/usr/bin/env python3
"""A stand-in for the model server, FOR REHEARSING THE SCRIPTS ONLY. It answers like an OpenAI-compatible server with a fixed sentence after a short delay.

    python experiments/server/mock_vllm.py [--port 8999] [--delay 0.05]

Results measured against it say nothing about a GPU; run_all.py --dry marks them as a rehearsal in every file and in the report banner.
"""
import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class H(BaseHTTPRequestHandler):
    delay = 0.05

    def log_message(self, *_):
        pass

    def _send(self, obj):
        data = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802
        self._send({'data': [{'id': 'mock-model'}]})

    def do_POST(self):  # noqa: N802
        n = int(self.headers.get('Content-Length') or 0)
        self.rfile.read(n)
        time.sleep(self.delay)
        words = 'This rule protects the payer from paying a wrong amount; check the line and the policy before confirming.'
        self._send({'choices': [{'message': {'role': 'assistant', 'content': words}}], 'usage': {'prompt_tokens': 250, 'completion_tokens': len(words.split())}})


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=8999)
    ap.add_argument('--delay', type=float, default=0.05)
    a = ap.parse_args()
    H.delay = a.delay
    ThreadingHTTPServer(('127.0.0.1', a.port), H).serve_forever()
