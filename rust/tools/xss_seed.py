"""Starts a demo server and plants script-injection payloads everywhere a person can put text that another person's browser will show:
claim fields (notes, ids, service codes, provider, line ids, attachment text), a user's display name, and failed sign-ins (the badge text
lands in the audit panel). Leaves the server running (kill it with the printed pid) so a real browser can open the console and look.

    python rust/tools/xss_seed.py [--port 8497]

Prints the demo accounts' file path and the markers the browser check looks for. Every payload sets window.__xss = 'PAYLOAD-NAME' if it
ever runs; the check afterwards asserts that window.__xss was never set and that no script or event-handler attribute exists in the page.
"""
import argparse, copy, json, os, subprocess, sys, tempfile, time, urllib.request, urllib.error, http.cookiejar
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[2]
CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')


def payloads():
    return {
        'img': '<img src=x onerror="window.__xss=\'img\'">',
        'script': '<script>window.__xss="script"</script>',
        'svg': '<svg onload="window.__xss=\'svg\'"></svg>',
        'attr-break': '"><img src=x onerror="window.__xss=\'attr\'">',
        'js-url': 'javascript:window.__xss="jsurl"',
        'template': '${window.__xss="tpl"}{{constructor.constructor("window.__xss=1")()}}',
        'iframe': '<iframe srcdoc="<script>parent.__xss=\'iframe\'</script>"></iframe>',
        'style': '<style>body{display:none}</style><link rel=stylesheet href=//evil.example/x.css>',
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=8497)
    a = ap.parse_args()
    data = Path(tempfile.mkdtemp(prefix='cg_xss_'))
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT))
    proc = subprocess.Popen([CG, 'serve', '--demo', '--port', str(a.port), '--data-dir', str(data)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(80):
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{a.port}/healthz', timeout=1)
            break
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    acc = json.loads((data / 'demo_accounts.json').read_text())

    def call(op, method, path, body=None, csrf=None):
        h = {'content-type': 'application/json'}
        if csrf:
            h['x-csrf-token'] = csrf
        try:
            r = op.open(urllib.request.Request(f'http://127.0.0.1:{a.port}{path}', json.dumps(body).encode() if body is not None else None, h, method=method), timeout=30)
            return r.status, json.loads(r.read() or b'null')
        except urllib.error.HTTPError as e:
            return e.code, e.read()[:200]

    def login(badge):
        op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        s, b = call(op, 'POST', '/api/v1/auth/login', {'badge_id': badge, 'password': acc[badge]['password'], 'totp': pyotp.TOTP(acc[badge]['secret']).now()})
        assert s == 200 and isinstance(b, dict) and 'csrf' in b, (s, b)
        return op, b['csrf']

    # failed sign-ins whose "badge" is a payload: they appear in the audit panel
    anon = urllib.request.build_opener()
    for name, text in payloads().items():
        call(anon, 'POST', '/api/v1/auth/login', {'badge_id': text[:60], 'password': 'x', 'totp': '000000'})
    admin = login('CG-4004')
    base = next(json.loads(l) for l in (ROOT / 'data' / 'development' / 'claims.jsonl').read_text(encoding='utf-8').splitlines() if '"SVC-' in l)
    claims = []
    for name, text in payloads().items():
        c = copy.deepcopy(base)
        c['claim_id'] = f'CG-XSS-{name.upper()}'
        c['notes'] = text
        c['invoice_number'] = text[:60]
        c['provider_id'] = text[:80]
        c['currency'] = text[:20]
        c['lines'][0]['service_code'] = text[:150]
        c['lines'][0]['line_id'] = text[:40]
        if c.get('attachments'):
            c['attachments'][0]['text'] = text
        claims.append(c)
    s, r = call(admin[0], 'POST', '/api/v1/queue/submit', {'claims': claims}, admin[1])
    print('claims submitted:', s, sum(1 for x in r['results'] if x.get('accepted')) if s == 200 else r)
    for i, (name, text) in enumerate(payloads().items()):
        s, r = call(admin[0], 'POST', '/api/v1/users', {'badge_id': f'CG-66{i:02d}', 'name': text[:90], 'password': 'Correct-Horse-9x', 'level': 1}, admin[1])
    s, cfg = call(admin[0], 'GET', '/api/v1/queue/config')
    print('shift:', call(admin[0], 'PATCH', '/api/v1/queue/config', {'expected_version': cfg['stored_version'], 'on_shift': ['CG-3004'], 'slice_size': 100, 'low_water': 90}, admin[1]))
    print(json.dumps({'port': a.port, 'pid': proc.pid, 'accounts': str(data / 'demo_accounts.json'), 'claims': [c['claim_id'] for c in claims]}))


if __name__ == '__main__':
    main()
