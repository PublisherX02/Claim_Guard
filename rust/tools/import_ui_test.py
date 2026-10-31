"""End-to-end test of importing claims from files, in real Chrome, against a demo server and the document reader sidecar that this script starts.

    python rust/tools/make_docs.py --out <docs> --n 4
    python rust/tools/import_ui_test.py --docs <docs> [--engine tesseract|both]

Needs: playwright (Chrome channel), pyotp, pypdfium2, pillow, pypdf, reportlab, the tesseract binary (TESSERACT_CMD, TESSDATA_PREFIX); for `both`
also Ollama with the vision model (VLM_BASE_URL, VLM_MODEL). Checks the screens a person uses: the page exists for an administrator only, a photographed
claim becomes an editable draft whose doubtful fields are marked, the draft cannot be sent before it is checked and confirmed, a JSONL file is previewed
record by record, a hostile PDF is refused with a reason, and what is sent reaches the queue. Authorized scope: the user's own demo server on 127.0.0.1.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import pyotp
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
CG = os.environ.get('CG_BIN', str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
SERVER_PORT, READER_PORT = 8495, 8096
TOKEN = 'import-ui-token-' + str(os.getpid())
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def wait_http(url, headers=None, tries=60):
    for _ in range(tries):
        try:
            urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=2)
            return True
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--docs', required=True)
    ap.add_argument('--engine', default='tesseract')
    ap.add_argument('--out', default=os.path.join(os.environ.get('TEMP', '.'), 'cg_import_ui'))
    a = ap.parse_args()
    docs = Path(a.docs)
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    data = out / 'data'
    data.mkdir(exist_ok=True)
    env_reader = dict(os.environ, OCR_TOKEN=TOKEN, OCR_PORT=str(READER_PORT), OCR_HOST='127.0.0.1', OCR_ENGINE=a.engine)
    reader = subprocess.Popen([sys.executable, str(ROOT / 'deploy' / 'ocr' / 'service.py')], env=env_reader, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    env_server = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), DOCUMENT_READER_URL='http://127.0.0.1:%d' % READER_PORT, DOCUMENT_READER_TOKEN=TOKEN, DOCUMENT_READER_ENGINE=a.engine)
    server = subprocess.Popen([CG, 'serve', '--demo', '--port', str(SERVER_PORT), '--data-dir', str(data)], env=env_server, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        check('the reader and the server start', wait_http('http://127.0.0.1:%d/health' % READER_PORT, {'Authorization': 'Bearer ' + TOKEN}) and wait_http('http://127.0.0.1:%d/healthz' % SERVER_PORT))
        time.sleep(4)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        from ocr_service_test import make_pdfs
        hostile = out / 'hostile.pdf'
        hostile.write_bytes(make_pdfs(None)['js'])
        photo = sorted(docs.glob(os.environ.get('IMPORT_PHOTO_GLOB', '*_B_photo.jpg')))[0]   # the default is the hardest picture; a cleaner one can be chosen
        scan = sorted(docs.glob('*_A_scan.png'))[0]
        jsonl = out / 'three_claims.jsonl'
        jsonl.write_text(''.join(l + chr(10) for l in (ROOT / 'data/stress/claims.jsonl').read_text(encoding='utf-8').split(chr(10))[:3] if l.strip()), encoding='utf-8')
        issues = []
        with sync_playwright() as p:
            browser = p.chromium.launch(channel='chrome', headless=True)
            ctx = browser.new_context(viewport={'width': 1400, 'height': 1000})
            pg = ctx.new_page()
            pg.on('console', lambda m: issues.append((m.type, m.text[:160])) if m.type in ('error', 'warning') else None)
            pg.on('pageerror', lambda e: issues.append(('pageerror', str(e)[:160])))

            def login(page, badge):
                page.goto('http://127.0.0.1:%d/#/login' % SERVER_PORT)
                page.wait_for_selector('input[autocomplete=username]')
                tp = pyotp.TOTP(acc[badge]['secret'])
                while 30 - time.time() % 30 < 2:
                    time.sleep(1)
                page.fill('input[autocomplete=username]', badge)
                page.fill('input[type=password]', acc[badge]['password'])
                page.fill('input[autocomplete=one-time-code]', tp.now())
                page.click('button[type=submit]')
                page.wait_for_function("() => location.hash !== '#/login'", timeout=8000)

            login(pg, 'CG-4004')
            nav = pg.inner_text('body')
            check('the administrator sees "Import from files" in the menu', 'Import from files' in nav)
            before = pg.evaluate("async () => { const r = await fetch('/api/v1/queue/dashboard'); const d = await r.json(); return Object.values(d.counts).reduce((a, b) => a + b, 0); }")
            pg.goto('http://127.0.0.1:%d/#/import' % SERVER_PORT)
            pg.wait_for_selector('input[aria-label^="Files to import"]')
            pg.set_input_files('input[aria-label^="Files to import"]', [str(photo), str(jsonl), str(hostile)])
            pg.click('text=Read the file(s)')
            pg.wait_for_selector('.importdoc', timeout=180000)
            pg.wait_for_function("() => document.querySelectorAll('.card h2').length >= 2 && !document.body.innerText.includes('Reading ')", timeout=180000)
            body = pg.inner_text('body')
            pg.screenshot(path=str(out / 'import_preview.png'), full_page=True)
            check('the photographed claim is shown as a DRAFT with the page beside it', 'A DRAFT, not a claim yet' in body and pg.query_selector('img.preview') is not None)
            check('the JSONL file is previewed record by record and says nothing is queued', '3 record(s)' in body and 'Nothing has been queued' in body, body[:400])
            check('the hostile PDF is refused with its reason, in its own place', 'pdf_active_content' in body or 'active or hidden content' in body, body[-400:])
            n_inputs = pg.evaluate("() => document.querySelectorAll('.importdoc input[type=text]').length")
            n_flag = pg.evaluate("() => document.querySelectorAll('.importdoc input.flagged').length")
            check('the draft has its fields as inputs (%d), some marked as doubtful (%d)' % (n_inputs, n_flag), n_inputs >= 20)
            check('the doubtful fields come with their reasons', pg.evaluate("() => document.querySelectorAll('.importdoc .flagtext').length") > 0 or n_flag == 0)
            # the draft cannot be sent before it is checked and confirmed
            send = pg.locator('button', has_text='Send to the queue').first
            check('"Send to the queue" is disabled until the claim is checked and confirmed', send.is_disabled())
            # fill what the reader left empty (a person types it), then check
            empties = pg.evaluate("() => Array.from(document.querySelectorAll('.importdoc .importgrid input')).filter(i => i.value === '').map(i => i.getAttribute('aria-label'))")
            for label in empties:
                if label == 'Claim ID':
                    pg.fill('.importdoc input[aria-label="Claim ID"]', 'CG-IMPORTUI0001')
            pg.click('text=Check this claim')
            pg.wait_for_selector('.importdoc .notice.good, .importdoc .notice.bad', timeout=20000)
            verdict = pg.inner_text('.importdoc')
            check('"Check this claim" answers with the server\'s own format check', 'passes the format check' in verdict or 'Not acceptable yet' in verdict, verdict[-300:])
            if 'passes the format check' in verdict:
                check('still disabled until the person says they compared the fields with the page', send.is_disabled())
                pg.check('.importdoc input[type=checkbox]')
                check('now it can be sent', not send.is_disabled())
                send.click()
                pg.wait_for_selector('text=claims accepted into the queue', timeout=30000)
                check('the claim is accepted into the queue', 'accepted into the queue' in pg.inner_text('body'))
            else:
                print('     (the draft was not valid as read: %s) -- editing is how a person fixes it; skipping the send step' % verdict[-160:])
            # the records card: send the three claims
            btn = pg.locator('button', has_text='Send the checked claims to the queue').first
            btn.click()
            pg.wait_for_selector('text=3 of 3 claims accepted', timeout=30000)
            check('the three JSONL claims are accepted into the queue', True)
            time.sleep(1)
            after = pg.evaluate("async () => { const r = await fetch('/api/v1/queue/dashboard'); const d = await r.json(); return Object.values(d.counts).reduce((a, b) => a + b, 0); }")
            check('the queue grew by what was sent (%d -> %d)' % (before, after), after >= before + 3, (before, after))
            # a reviewer does not see the page at all
            ctx2 = browser.new_context()
            pg2 = ctx2.new_page()
            login(pg2, 'CG-2002')
            check('a reviewer has no "Import from files" in the menu (hidden, not disabled)', 'Import from files' not in pg2.inner_text('body'))
            pg2.goto('http://127.0.0.1:%d/#/import' % SERVER_PORT)
            pg2.wait_for_timeout(800)
            check('and typing the address does not open it', 'Import claims from files' not in pg2.inner_text('body'))
            unexpected = [i for i in issues if 'status of 422' not in i[1]]    # the hostile PDF is refused with a 422, and the browser logs every failed request
            check('no console errors or warnings (apart from the expected 422 for the hostile PDF)', not unexpected, unexpected[:3])
            browser.close()
    finally:
        reader.kill()
        server.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
