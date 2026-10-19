#!/usr/bin/env python3
"""cg-embed: the embedding sidecar for "Ask the rulebook" (docs/52). One job: turn short texts into unit vectors.

    POST /embed    JSON {"texts": ["...", ...]}  (at most 128 texts of at most 2000 characters)  -> {"model", "dim", "vectors": [[...], ...]}
    GET  /health   -> {"ok": true, "model", "dim"}      (both need  Authorization: Bearer <EMBED_TOKEN>)

It sees only the text of the rulebook passages and the reviewer's question, never a claim. It makes no outbound call: the model is baked into the image at a
pinned revision and the process runs with HF_HUB_OFFLINE=1.

The vectors must be exactly what the experiment measured: sentence-transformers, `encode(..., normalize_embeddings=True)`. The refusal threshold in the server
(EMBEDDER_THRESHOLD, 0.485) was chosen on those vectors (BAAI/bge-m3, max 256 tokens, docs/55); a different model, runtime or pooling needs its own threshold and a new run of rust/tools/rag_parity.py.

Environment: EMBED_TOKEN (required), EMBED_MODEL (default BAAI/bge-m3; the earlier sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2 needs threshold 0.415), EMBED_DEVICE (default auto: the GPU in half precision when torch sees one, else cpu; cpu / cuda force it),
EMBED_HOST (127.0.0.1), EMBED_PORT (8091).
"""
import hmac
import http.server
import json
import os
import sys
import threading

MODEL_ID = os.environ.get('EMBED_MODEL', 'BAAI/bge-m3')
DEVICE = os.environ.get('EMBED_DEVICE', 'auto')
MAX_TEXTS, MAX_CHARS, MAX_BODY = 128, 2000, 512 * 1024
TOKEN = os.environ.get('EMBED_TOKEN', '')
LOCK = threading.Lock()          # one encode at a time: the model is not thread-safe and the box is small
MODEL = None


def load():
    global MODEL
    from sentence_transformers import SentenceTransformer
    device = DEVICE
    if device == 'auto':
        import torch
        device = 'cuda' if torch.cuda.is_available() else 'cpu'
    MODEL = SentenceTransformer(MODEL_ID, device=device)
    if device.startswith('cuda'):
        MODEL.half()                    # the experiment (docs/55) ran fp16 on a GPU; the 0.485 threshold was chosen on those vectors
    MODEL.max_seq_length = 256          # what the experiment measured; passages and questions are short
    return MODEL


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = 'cg-embed'
    protocol_version = 'HTTP/1.1'

    def log_message(self, *a):          # no request log: the texts are the content
        pass

    def _send(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _authorised(self):
        got = self.headers.get('Authorization', '')
        return got.startswith('Bearer ') and hmac.compare_digest(got[7:].encode(), TOKEN.encode())

    def do_GET(self):
        if self.path != '/health':
            return self._send(404, {'error': 'not_found'})
        if not self._authorised():
            return self._send(401, {'error': 'unauthorized'})
        self._send(200, {'ok': True, 'model': MODEL_ID, 'dim': MODEL.get_sentence_embedding_dimension()})

    def do_POST(self):
        if self.path != '/embed':
            return self._send(404, {'error': 'not_found'})
        if not self._authorised():
            return self._send(401, {'error': 'unauthorized'})
        try:
            n = int(self.headers.get('Content-Length', '0'))
        except ValueError:
            return self._send(400, {'error': 'bad_length'})
        if n <= 0 or n > MAX_BODY:
            return self._send(413, {'error': 'body_size'})
        try:
            texts = json.loads(self.rfile.read(n))['texts']
        except Exception:  # noqa: BLE001
            return self._send(400, {'error': 'bad_json'})
        if not isinstance(texts, list) or not 1 <= len(texts) <= MAX_TEXTS or not all(isinstance(t, str) and len(t) <= MAX_CHARS for t in texts):
            return self._send(422, {'error': 'texts', 'detail': 'send 1 to %d strings of at most %d characters' % (MAX_TEXTS, MAX_CHARS)})
        with LOCK:
            vecs = MODEL.encode([t if t.strip() else ' ' for t in texts], normalize_embeddings=True, batch_size=64)
        self._send(200, {'model': MODEL_ID, 'dim': int(vecs.shape[1]), 'vectors': [[float(x) for x in v] for v in vecs]})


def main():
    if not TOKEN:
        sys.exit('EMBED_TOKEN is required')
    load()
    host, port = os.environ.get('EMBED_HOST', '127.0.0.1'), int(os.environ.get('EMBED_PORT', '8091'))
    srv = http.server.ThreadingHTTPServer((host, port), Handler)
    print('cg-embed on %s:%d, model %s on %s' % (host, port, MODEL_ID, DEVICE), flush=True)
    srv.serve_forever()


if __name__ == '__main__':
    main()
