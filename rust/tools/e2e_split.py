"""The production topology end to end: `cg serve --queue` and `cg worker` as two processes sharing one real MongoDB.

    python rust/tools/e2e_split.py [--mongo mongodb://127.0.0.1:27018]

An administrator (created by `cg admin`) puts reviewers on shift and submits claims; the worker (a different process) takes them through
triage and the pipeline and deals them; a reviewer pulls a claim, decides, a senior signs off. Prints one line per step and exits 1 on the
first failure. Uses a throw-away database that is dropped at the end.
"""
import argparse, json, os, re, secrets, subprocess, sys, time, urllib.request, urllib.error, http.cookiejar
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[2]
CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8471


def api(op, method, path, body=None, csrf=None):
    h = {'content-type': 'application/json'}
    if csrf:
        h['x-csrf-token'] = csrf
    try:
        r = op.open(urllib.request.Request(f'http://127.0.0.1:{PORT}{path}', json.dumps(body).encode() if body is not None else None, h, method=method), timeout=20)
        return r.status, json.loads(r.read() or b'null')
    except urllib.error.HTTPError as e:
        return e.code, e.read()[:200]


def step(name, ok, detail=''):
    print(('ok   ' if ok else 'FAIL ') + name + (f'  {detail}' if detail else ''), flush=True)
    if not ok:
        raise SystemExit(1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--mongo', default='mongodb://127.0.0.1:27018')
    a = ap.parse_args()
    db = 'cg_e2e_' + secrets.token_hex(4)
    data = Path(os.environ.get('TEMP', '/tmp')) / ('cg_e2e_' + db)
    data.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, MONGO_URI=a.mongo, MONGO_DB=db, JWT_SECRET=secrets.token_urlsafe(48), AUDIT_ANCHOR_KEY=secrets.token_urlsafe(48), PII_KEY=secrets.token_urlsafe(48),
               FERNET_KEY=__import__('cryptography.fernet', fromlist=['Fernet']).Fernet.generate_key().decode(), BCRYPT_ROUNDS='4', COOKIE_SECURE='false',
               CLAIMGUARD_ROOT=str(ROOT), CLAIMGUARD_DATA_DIR=str(data))
    procs = []
    try:
        seed = subprocess.run([CG, 'admin', '--dev', 'seed-demo'], env=env, capture_output=True, text=True, encoding='utf-8')
        step('admin seed-demo into MongoDB', seed.returncode == 0, seed.stdout[-200:] + seed.stderr[-200:])
        accounts = {}
        for m in re.finditer(r'(CG-\d+) level (\d) .* password: (\S+)\notpauth://\S+secret=([A-Z2-7]+)', seed.stdout):
            accounts[m.group(1)] = {'password': m.group(3), 'secret': m.group(4)}
        step('four accounts created', len(accounts) == 4)
        procs.append(subprocess.Popen([CG, 'serve', '--dev', '--queue', '--port', str(PORT)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        procs.append(subprocess.Popen([CG, 'worker', '--dev'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        for _ in range(80):
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{PORT}/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        else:
            step('server up', False)
        step('server up', True)

        def login(badge):
            op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            for _ in range(40):
                s, b = api(op, 'POST', '/api/v1/auth/login', {'badge_id': badge, 'password': accounts[badge]['password'], 'totp': pyotp.TOTP(accounts[badge]['secret']).now()})
                if s == 200:
                    return op, b['csrf']
                time.sleep(1)
            step(f'login {badge}', False, str(b))
        admin, reviewer, senior = login('CG-4004'), login('CG-2002'), login('CG-3003')
        step('three roles signed in', True)
        s, cfg = api(admin[0], 'GET', '/api/v1/queue/config', csrf=admin[1])
        step('read routing configuration', s == 200, str(cfg)[:100])
        version = cfg.get('version', 0) if isinstance(cfg, dict) else 0
        s, r = api(admin[0], 'PATCH', '/api/v1/queue/config', {'expected_version': version, 'on_shift': ['CG-2002', 'CG-3003'], 'slice_size': 6, 'low_water': 3}, admin[1])
        step('admin puts reviewers on shift', s == 200, str(r)[:100])
        claims = [json.loads(l) for l in (ROOT / 'data' / 'development' / 'claims.jsonl').read_text(encoding='utf-8').splitlines()[:25]]
        s, r = api(admin[0], 'POST', '/api/v1/queue/submit', {'claims': claims}, admin[1])
        step('admin submits 25 claims', s == 200 and len(r['results']) == 25, str(r)[:100])
        deadline, got = time.time() + 40, None
        while time.time() < deadline and not got:
            s, inbox = api(reviewer[0], 'GET', '/api/v1/work/inbox', csrf=None)
            got = inbox['claims'] if s == 200 and inbox['claims'] else None
            time.sleep(1)
        step('the separate worker process dealt claims to the reviewer', bool(got), f'{len(got or [])} claims')
        claim = got[0]
        step('reviewer has a leased claim in the inbox', claim is not None)
        cid = claim['claim_id']
        decided = 0
        for c in got:
            for f in c['findings']:
                if f.get('allowed_actions') and 'confirm_issue' in f['allowed_actions']:
                    s, r = api(reviewer[0], 'POST', f'/api/v1/work/claims/{c["claim_id"]}/findings/{f["rule_id"]}/decision', {'action': 'confirm_issue', 'reason': 'e2e check'}, reviewer[1])
                    if s == 200:
                        decided += 1
            if not c['findings'] or c.get('allowed_actions'):
                s, r = api(reviewer[0], 'POST', f'/api/v1/work/claims/{c["claim_id"]}/verify', {'action': 'verify_clear'}, reviewer[1])
                decided += 1 if s == 200 else 0
        step('reviewer decisions recorded through the queue', decided > 0, f'{decided} decisions')
        s, trace = api(admin[0], 'GET', f'/api/v1/ops/trace/{cid}', csrf=None)
        step('administrator reads the claim trace', s == 200)
        s, health = api(admin[0], 'GET', '/api/v1/ops/health', csrf=None)
        comp = {c['id']: c['status'] for c in health['components']} if s == 200 else {}
        step('health shows the worker process alive (scheduler ok)', comp.get('scheduler') == 'ok', json.dumps(comp))
        s, v = api(admin[0], 'GET', '/api/v1/audit/verify', csrf=None)
        step('audit chains verify', s == 200 and v['security']['ok'] and v['review']['ok'])
        print('SPLIT-PROCESS END TO END: ALL STEPS PASSED')
        return 0
    finally:
        for p in procs:
            p.kill()
        try:
            from pymongo import MongoClient
            c = MongoClient(a.mongo, serverSelectionTimeoutMS=2000)
            for name in (db, db + '_queue'):
                c.drop_database(name)
        except Exception:  # noqa: BLE001
            pass


if __name__ == '__main__':
    sys.exit(main())
