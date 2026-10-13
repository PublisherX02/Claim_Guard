"""Black-box test of the document reader sidecar (deploy/ocr/service.py), which it starts itself on a test port.

    python rust/tools/ocr_service_test.py [--tesseract PATH] [--tessdata DIR]

Needs: pip install pypdfium2 pillow pypdf reportlab, and the tesseract binary. Checks: no token / wrong token refused; a digital PDF is read from its text
layer (no OCR); an image-only PDF and a photo are OCR'd with per-word confidence; hostile PDFs (JavaScript, launch action, open action, embedded file,
encrypted) are refused with 422 before anything is rendered; garbage, wrong types, oversize bodies and too many pages are refused; one bad request does
not stop the next. Authorized scope: the user's own service on 127.0.0.1 with synthetic documents.
"""
import argparse
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PORT = 8091
TOKEN = 'ocr-test-token-' + str(os.getpid())
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def call(path, data=None, ctype=None, token=TOKEN, extra=None):
    h = {}
    if token:
        h['Authorization'] = 'Bearer ' + token
    if ctype:
        h['Content-Type'] = ctype
    h.update(extra or {})
    req = urllib.request.Request('http://127.0.0.1:%d%s' % (PORT, path), data=data, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b'{}')


def make_pdfs(tmp):
    from pypdf import PdfReader, PdfWriter
    from pypdf.generic import DictionaryObject, NameObject, TextStringObject
    from reportlab.lib.pagesizes import A4
    from reportlab.pdfgen import canvas
    from PIL import Image, ImageDraw, ImageFont
    out = {}
    buf = io.BytesIO()
    c = canvas.Canvas(buf, pagesize=A4)
    c.setFont('Helvetica', 14)
    c.drawString(60, 760, 'CLAIM FORM  Claim ID: CG-TEST000001')
    c.drawString(60, 740, 'Patient ID: PAT-0001   Total amount: 150.00 SAR')
    c.save()
    out['digital'] = buf.getvalue()
    img = Image.new('RGB', (1240, 600), 'white')
    d = ImageDraw.Draw(img)
    try:
        f = ImageFont.truetype('arial.ttf', 44)
    except OSError:
        f = ImageFont.load_default()
    d.text((60, 80), 'CLAIM FORM  Claim ID: CG-TEST000002', fill='black', font=f)
    d.text((60, 180), 'Patient ID: PAT-0002   Total amount: 275.50 SAR', fill='black', font=f)
    b = io.BytesIO()
    img.save(b, 'PNG')
    out['png'] = b.getvalue()
    b = io.BytesIO()
    img.save(b, 'PDF', resolution=150)
    out['scanpdf'] = b.getvalue()
    base = PdfReader(io.BytesIO(out['digital']))

    def variant(fn, encrypt=False):
        w = PdfWriter()
        w.append(base)
        fn(w)
        if encrypt:
            w.encrypt('secret')
        o = io.BytesIO()
        w.write(o)
        return o.getvalue()

    out['js'] = variant(lambda w: w.add_js('app.alert(1)'))   # deprecated in pypdf 7 but still the plain way to build the test file
    out['openaction'] = variant(lambda w: w._root_object.update({NameObject('/OpenAction'): DictionaryObject({NameObject('/S'): NameObject('/JavaScript'), NameObject('/JS'): TextStringObject('1')})}))
    out['launch'] = variant(lambda w: w._root_object.update({NameObject('/AA'): DictionaryObject({NameObject('/O'): DictionaryObject({NameObject('/S'): NameObject('/Launch'), NameObject('/F'): TextStringObject('cmd.exe')})})}))
    out['embedded'] = variant(lambda w: w.add_attachment('x.txt', b'hello'))
    out['encrypted'] = variant(lambda w: None, encrypt=True)
    many = PdfWriter()
    for _ in range(35):
        many.append(base)
    o = io.BytesIO()
    many.write(o)
    out['toomany'] = o.getvalue()
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--tesseract', default=os.environ.get('TESSERACT_CMD', 'tesseract'))
    ap.add_argument('--tessdata', default=os.environ.get('TESSDATA_PREFIX', ''))
    ap.add_argument('--container', default='', help='test this image (e.g. claimguard-reader:latest) run with the production flags instead of the local script')
    a = ap.parse_args()
    env = dict(os.environ, OCR_TOKEN=TOKEN, OCR_PORT=str(PORT), OCR_HOST='127.0.0.1', OCR_ENGINE='tesseract', TESSERACT_CMD=a.tesseract, OCR_MAX_BYTES=str(2 * 1024 * 1024))
    if a.tessdata:
        env['TESSDATA_PREFIX'] = a.tessdata
    cname = 'cg-reader-test-%d' % os.getpid()
    if a.container:
        # the same restrictions as deploy/rust/compose.yml: read-only, no capabilities, no new privileges, memory/process/cpu limits
        proc = subprocess.Popen(['docker', 'run', '--rm', '--name', cname, '--read-only', '--tmpfs', '/tmp', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', '--memory', '2g', '--pids-limit', '128', '--cpus', '2',
                                 '-e', 'OCR_TOKEN=' + TOKEN, '-e', 'OCR_ENGINE=tesseract', '-e', 'OCR_MAX_BYTES=%d' % (2 * 1024 * 1024), '-p', '127.0.0.1:%d:8090' % PORT, a.container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    else:
        proc = subprocess.Popen([sys.executable, str(ROOT / 'deploy' / 'ocr' / 'service.py')], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(40):
            try:
                if call('/health')[0] == 200:
                    break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        docs = make_pdfs(None)
        check('no token: 401', call('/health', token=None)[0] == 401)
        check('wrong token: 401', call('/read', b'x', 'application/pdf', token='nope')[0] == 401)
        st, out = call('/read', docs['digital'], 'application/pdf')
        check('a digital PDF is read from its text layer, no OCR', st == 200 and out['pages'][0]['source'] == 'text_layer' and 'CG-TEST000001' in out['pages'][0]['text'], (st, out))
        st, out = call('/read', docs['scanpdf'], 'application/pdf')
        # Tesseract builds differ on the zero/letter-O confusion in ids; the extractor's catalogue and confidence checks exist for that
        check('an image-only PDF is OCR\'d, with per-word confidence', st == 200 and out['pages'][0]['source'] == 'ocr' and 'CLAIM FORM' in out['pages'][0]['text'] and 'PAT-0002' in out['pages'][0]['text'].replace('O', '0') and out['pages'][0]['words'] and out['pages'][0]['words'][0]['conf'] is not None, (st, str(out)[:200]))
        st, out = call('/read', docs['png'], 'image/png')
        check('a PNG is OCR\'d and the amount is right', st == 200 and '275.50' in out['pages'][0]['text'], (st, str(out)[:200]))
        for name in ('js', 'openaction', 'launch', 'embedded', 'encrypted'):
            st, out = call('/read', docs[name], 'application/pdf')
            check('hostile PDF (%s) is refused with 422 before rendering' % name, st == 422 and ('pdf_active_content' in out.get('error', '') or 'encrypted' in out.get('error', '')), (st, out))
        st, out = call('/read', docs['toomany'], 'application/pdf')
        check('35 pages: refused (limit 30)', st == 422 and 'pages' in out.get('error', ''), (st, out))
        st, out = call('/read', b'%PDF-1.4 garbage', 'application/pdf')
        check('a broken PDF is refused with 422', st == 422, (st, out))
        st, out = call('/read', b'not an image', 'image/png')
        check('a broken image is refused with 422', st == 422, (st, out))
        check('an unsupported type is 415', call('/read', b'<html>', 'text/html')[0] == 415)
        try:
            st, out = call('/read', b'0' * (3 * 1024 * 1024), 'application/pdf')
        except (ConnectionError, OSError):
            st, out = 413, 'connection closed by the server while the body was still being sent'   # refused before reading it: also correct
        check('a body over the limit is refused (413 or the connection closed)', st == 413, (st, out))
        check('an unknown language list is 400', call('/read', docs['png'], 'image/png', extra={'X-Langs': 'eng;rm -rf'})[0] == 400)
        check('an unknown engine is 400', call('/read', docs['png'], 'image/png', extra={'X-Engine': 'magic'})[0] == 400)
        check('the service still answers after all that', call('/health')[0] == 200)
    finally:
        if a.container:
            subprocess.run(['docker', 'rm', '-f', cname], capture_output=True)
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
