#!/usr/bin/env python3
"""cg-ocr: the document reader sidecar. One job: turn a PDF page or an image into text, and say how it was read.

    POST /read     body = the raw file; Content-Type: application/pdf | image/png | image/jpeg | image/tiff | image/webp
                   optional headers: X-Engine: tesseract | vlm | auto,  X-Langs: eng+ara
                   -> {"engine", "pages": [{"page", "text", "source", "words": [{"text","conf","box"}]}], "warnings": []}
    GET  /health   -> which engines are available

Why a separate process: rasterizing a hostile PDF is the riskiest thing the system does with an uploaded file. It runs here, in its own container
(no network except the model server, a read-only file system, a memory and time limit, one request at a time), after the main server has already
refused anything that fails its gate. The main server never parses a scan or draws a PDF itself.

Engines:
  tesseract  the Tesseract command line (CPU, offline, per-word confidence, no model download)
  vlm        any OpenAI-compatible vision model server (llama.cpp llama-server, vLLM, SGLang, Ollama /v1): Unlimited-OCR, gemma3, and so on;
             no per-word confidence
  auto       vlm when VLM_BASE_URL is set, otherwise tesseract

Environment: OCR_TOKEN (required; the main server sends it as a bearer token), OCR_ENGINE (default auto), TESSERACT_CMD, TESSDATA_PREFIX, OCR_LANGS,
VLM_BASE_URL, VLM_MODEL, VLM_PROMPT, OCR_MAX_PAGES (30), OCR_MAX_BYTES (16 MB), OCR_MAX_PIXELS (40 million), OCR_DPI (200), OCR_PAGE_TIMEOUT (120 s), OCR_PORT.
Dependencies: pypdfium2 and Pillow (pip), the tesseract binary.
"""
import base64
import hmac
import http.server
import io
import json
import os
import re
import shutil
import subprocess
import tempfile
import threading
import urllib.request

import pypdfium2 as pdfium
from PIL import Image

Image.MAX_IMAGE_PIXELS = None          # checked by hand below, with our own cap
MAX_BYTES = int(os.environ.get('OCR_MAX_BYTES', 16 * 1024 * 1024))
MAX_PAGES = int(os.environ.get('OCR_MAX_PAGES', 30))
MAX_PIXELS = int(os.environ.get('OCR_MAX_PIXELS', 40_000_000))
DPI = int(os.environ.get('OCR_DPI', 200))
PAGE_TIMEOUT = int(os.environ.get('OCR_PAGE_TIMEOUT', 120))
TESSERACT = os.environ.get('TESSERACT_CMD', 'tesseract')
VLM_URL = os.environ.get('VLM_BASE_URL', '').rstrip('/')
VLM_MODEL = os.environ.get('VLM_MODEL', '')
VLM_PROMPT = os.environ.get('VLM_PROMPT', '<|grounding|>Convert the document to markdown.')
VLM_MAX_TOKENS = int(os.environ.get('VLM_MAX_TOKENS', 3072))
TOKEN = os.environ.get('OCR_TOKEN', '')
LANGS_OK = re.compile(r'^[a-z]{3}(\+[a-z]{3})*$')
LOCK = threading.Lock()                 # one document at a time: memory stays bounded
DET = re.compile(r'<\|det\|>[^<]*<\|/det\|>')
REGION = re.compile(r'^(?:title|sub_title|text|table|list|formula|footer|header|caption|figure|image|page_number)\s*\[\d+,\s*\d+,\s*\d+,\s*\d+\]\s*', re.I)
LOOP = re.compile(r'(.)\1{39,}')
REF = re.compile(r'<\|/?ref\|>')


class Reject(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status, self.message = status, message


def tesseract_available():
    return shutil.which(TESSERACT) is not None or os.path.exists(TESSERACT)


BANNED_KEYS = {'/JS', '/JavaScript', '/OpenAction', '/AA', '/XFA', '/RichMedia', '/EmbeddedFiles', '/Launch'}
BANNED_ACTIONS = {'/JavaScript', '/Launch', '/SubmitForm', '/ImportData', '/GoToR', '/GoToE', '/Rendition', '/Movie', '/Sound'}
MIN_TEXT = 20          # a page with fewer non-space characters than this in its text layer is read as a picture


def pdf_features(body):
    """Active or hidden content reachable from the document: what a scanned claim never legitimately needs. A list of names; empty means clean."""
    from pypdf import PdfReader
    try:
        reader = PdfReader(io.BytesIO(body), strict=False)
        if reader.is_encrypted:
            return ['encrypted']
        found, seen, stack, budget = set(), set(), [reader.trailer], 300000
        while stack and budget > 0:
            obj = stack.pop()
            budget -= 1
            if hasattr(obj, 'get_object'):
                key = (getattr(obj, 'idnum', None), getattr(obj, 'generation', None))
                if key != (None, None):
                    if key in seen:
                        continue
                    seen.add(key)
                obj = obj.get_object()
            if isinstance(obj, dict):
                for k, v in obj.items():
                    if k in BANNED_KEYS:
                        found.add(str(k).lstrip('/'))
                    if k == '/S' and str(v) in BANNED_ACTIONS:
                        found.add(str(v).lstrip('/'))
                    if k == '/Type' and str(v) == '/EmbeddedFile':
                        found.add('EmbeddedFile')
                    stack.append(v)
            elif isinstance(obj, list):
                stack.extend(obj)
        if budget <= 0:
            found.add('too_complex')
        return sorted(found)
    except Exception as e:  # noqa: BLE001
        return ['unparseable: %s' % type(e).__name__]


def pages_of(body, ctype):
    """-> list of (PIL image or None, text-layer text or None), one per page, capped in count and size."""
    if ctype == 'application/pdf':
        features = pdf_features(body)
        if features:
            raise Reject(422, 'pdf_active_content: ' + ', '.join(features))
        try:
            pdf = pdfium.PdfDocument(body)
        except Exception as e:  # noqa: BLE001
            raise Reject(422, 'the PDF could not be opened: %s' % str(e)[:120])
        n = len(pdf)
        if n == 0 or n > MAX_PAGES:
            raise Reject(422, 'the PDF has %d pages (limit %d)' % (n, MAX_PAGES))
        out = []
        for i in range(n):
            page = pdf[i]
            text = page.get_textpage().get_text_range() or ''
            if len(re.sub(r'\s', '', text)) >= MIN_TEXT:
                out.append((None, text))
                continue
            w, h = page.get_size()
            scale = DPI / 72
            if w * scale * h * scale > MAX_PIXELS:
                raise Reject(422, 'page %d is too large to render' % (i + 1))
            out.append((page.render(scale=scale).to_pil().convert('RGB'), None))
        pdf.close()
        return out
    if ctype.startswith('image/'):
        try:
            img = Image.open(io.BytesIO(body))
            if img.width * img.height > MAX_PIXELS:
                raise Reject(422, 'the image is larger than %d pixels' % MAX_PIXELS)
            img.load()
        except Reject:
            raise
        except Exception as e:  # noqa: BLE001
            raise Reject(422, 'the image could not be read: %s' % str(e)[:120])
        return [(img.convert('RGB'), None)]
    raise Reject(415, 'unsupported content type')


PREPROCESS = os.environ.get('OCR_PREPROCESS', 'up')    # none | grey | up | grey+up: `up` measured best (docs/45); `grey` made Tesseract worse


def prepare_for_tesseract(img):
    """Optional preparation (OCR_PREPROCESS): `grey` = grey and stretched contrast, `up` = upscale a page narrower than 1800 pixels (never beyond a width of
    2400), `grey+up` = both. Nothing is cropped or rotated, so word positions stay meaningful. What helps was measured, not assumed (docs/45)."""
    from PIL import ImageOps
    if 'grey' in PREPROCESS:
        img = ImageOps.autocontrast(img.convert('L'), cutoff=1)
    if 'up' in PREPROCESS and img.width < 1800:
        factor = min(2.0, 2400 / img.width)
        img = img.resize((int(img.width * factor), int(img.height * factor)), Image.LANCZOS)
    return img


def run_tesseract(img, langs):
    """text and per-word confidence from the TSV output; the text keeps line breaks."""
    img = prepare_for_tesseract(img)
    with tempfile.TemporaryDirectory(prefix='cgocr_') as d:
        src = os.path.join(d, 'page.png')
        img.save(src, 'PNG')
        env = dict(os.environ, OMP_THREAD_LIMIT='1')
        try:
            p = subprocess.run([TESSERACT, src, 'stdout', '-l', langs, '--psm', '6', 'tsv'], capture_output=True, text=True, timeout=PAGE_TIMEOUT, env=env, encoding='utf-8', errors='replace')
        except subprocess.TimeoutExpired:
            raise Reject(504, 'tesseract timed out')
        if p.returncode != 0:
            raise Reject(502, 'tesseract failed: %s' % p.stderr[-160:])
    lines, words, last = [], [], None
    for row in p.stdout.split('\n')[1:]:
        cols = row.split('\t')
        if len(cols) < 12 or not cols[11].strip():
            continue
        key = (cols[1], cols[2], cols[3], cols[4])
        conf = float(cols[10])
        word = {'text': cols[11], 'conf': conf / 100 if conf >= 0 else None, 'box': [int(cols[6]), int(cols[7]), int(cols[6]) + int(cols[8]), int(cols[7]) + int(cols[9])]}
        words.append(word)
        if key != last:
            lines.append([])
            last = key
        lines[-1].append(cols[11])
    return '\n'.join(' '.join(l) for l in lines), words


def clean_vlm(text):
    """Layout markers removed, one region per line, an exact repeat of the previous line dropped, a looping answer refused (a run of 40 equal characters
    means the model fell into its repetition failure: the text is not to be trusted)."""
    text = REF.sub('', DET.sub('', text))
    if LOOP.search(text):
        raise Reject(502, 'the vision model looped (a long run of one character) and its answer was discarded')
    lines, last = [], None
    for raw in text.splitlines():
        line = REGION.sub('', raw).strip()
        if line.lower().startswith('<table>'):
            line = '[table] ' + re.sub(r'</?[a-z]+>', '', line)
        if not line or line == last:
            continue
        lines.append(line)
        last = line
    return chr(10).join(lines)


def run_vlm(img):
    if not VLM_URL or not VLM_MODEL:
        raise Reject(503, 'no vision model is configured (VLM_BASE_URL, VLM_MODEL)')
    buf = io.BytesIO()
    img.save(buf, 'PNG')
    data = 'data:image/png;base64,' + base64.b64encode(buf.getvalue()).decode()
    body = json.dumps({'model': VLM_MODEL, 'temperature': 0, 'max_tokens': VLM_MAX_TOKENS, 'stream': False,
                       'messages': [{'role': 'user', 'content': [{'type': 'text', 'text': VLM_PROMPT}, {'type': 'image_url', 'image_url': {'url': data}}]}]}).encode()
    req = urllib.request.Request(VLM_URL + '/chat/completions', data=body, headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + os.environ.get('VLM_API_KEY', 'ollama-local')})
    try:
        with urllib.request.urlopen(req, timeout=PAGE_TIMEOUT) as r:
            out = json.loads(r.read())
        text = out['choices'][0]['message']['content'] or ''
    except Exception as e:  # noqa: BLE001
        raise Reject(502, 'the vision model failed: %s' % str(e)[:160])
    return clean_vlm(text), []


def read(body, ctype, engine, langs):
    if engine == 'auto':
        engine = 'both' if VLM_URL and tesseract_available() else ('vlm' if VLM_URL else 'tesseract')
    if engine == 'tesseract' and not tesseract_available():
        raise Reject(503, 'tesseract is not installed')
    if engine not in ('tesseract', 'vlm', 'both'):
        raise Reject(400, 'unknown engine')
    if not LANGS_OK.match(langs):
        raise Reject(400, 'bad language list')
    pages, warnings = [], []
    for i, (img, layer) in enumerate(pages_of(body, ctype), start=1):
        if layer is not None:
            pages.append({'page': i, 'text': chr(10).join(layer.splitlines()), 'source': 'text_layer', 'words': []})
            continue
        if engine == 'both':
            # two independent readers: the vision model's text is the main text, Tesseract's is the second reading (table rows, word confidence, and a
            # cross-check: wherever the two disagree the main server flags the field). If the vision model fails on a page, the page is Tesseract's alone.
            second, words = run_tesseract(img, langs)
            try:
                text, _ = run_vlm(img)
                pages.append({'page': i, 'text': text, 'second_text': second, 'source': 'ocr', 'words': words})
            except Reject as r:
                warnings.append('page %d: %s; read by Tesseract alone' % (i, r.message))
                pages.append({'page': i, 'text': second, 'source': 'ocr', 'words': words})
            continue
        text, words = run_tesseract(img, langs) if engine == 'tesseract' else run_vlm(img)
        pages.append({'page': i, 'text': text, 'source': 'ocr', 'words': words})
    name = {'tesseract': 'tesseract', 'vlm': 'vlm:' + VLM_MODEL, 'both': 'vlm:' + VLM_MODEL + '+tesseract'}[engine]
    return {'engine': name, 'pages': pages, 'warnings': warnings}


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = 'cg-ocr'
    protocol_version = 'HTTP/1.1'

    def _send(self, status, obj):
        data = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(data)

    def _authed(self):
        got = self.headers.get('Authorization', '')
        return bool(TOKEN) and hmac.compare_digest(got.encode(), ('Bearer ' + TOKEN).encode())

    def do_GET(self):
        if self.path != '/health':
            return self._send(404, {'error': 'not_found'})
        if not self._authed():
            return self._send(401, {'error': 'unauthorized'})
        self._send(200, {'tesseract': tesseract_available(), 'vlm': bool(VLM_URL and VLM_MODEL), 'vlm_model': VLM_MODEL or None, 'max_pages': MAX_PAGES, 'dpi': DPI})

    def do_POST(self):
        if self.path != '/read':
            return self._send(404, {'error': 'not_found'})
        if not self._authed():
            return self._send(401, {'error': 'unauthorized'})
        try:
            n = int(self.headers.get('Content-Length', ''))
        except ValueError:
            return self._send(411, {'error': 'length_required'})
        if n <= 0 or n > MAX_BYTES:
            self.close_connection = True
            return self._send(413, {'error': 'payload_too_large'})
        body = self.rfile.read(n)
        ctype = (self.headers.get('Content-Type') or '').split(';')[0].strip().lower()
        engine = (self.headers.get('X-Engine') or os.environ.get('OCR_ENGINE', 'auto')).strip().lower()
        langs = (self.headers.get('X-Langs') or os.environ.get('OCR_LANGS', 'eng')).strip().lower()
        try:
            with LOCK:
                result = read(body, ctype, engine, langs)
        except Reject as r:
            return self._send(r.status, {'error': r.message})
        except Exception as e:  # noqa: BLE001
            return self._send(500, {'error': 'reader_failed', 'detail': type(e).__name__})
        self._send(200, result)

    def log_message(self, fmt, *args):
        pass


def main():
    if not TOKEN:
        raise SystemExit('OCR_TOKEN is not set')
    port = int(os.environ.get('OCR_PORT', '8090'))
    host = os.environ.get('OCR_HOST', '0.0.0.0')
    http.server.ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == '__main__':
    main()
