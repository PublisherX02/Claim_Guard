"""Does the audit protection notice a quiet rewrite of a log? Runs a real server with the mirror and the verification job on a short
interval, then edits an old row of the security log in place, as an intruder with file access would.

    python rust/tools/audit_guard_demo.py

Expected: before the edit the health panel's "Audit log protection" is ok; after it, within a few seconds, it is DOWN, the mirror
job names the first differing region, and the mirror still holds the original. Exit 0 only if all of that happens.
"""
import json, os, subprocess, sys, tempfile, time, urllib.request, http.cookiejar, urllib.error
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[2]
CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8493


def api(op, path):
    try:
        r = op.open(urllib.request.Request(f'http://127.0.0.1:{PORT}{path}'), timeout=10)
        return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def main():
    tmp = Path(tempfile.mkdtemp(prefix='cg_guard_'))
    data, mirror = tmp / 'data', tmp / 'mirror'
    data.mkdir()
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), AUDIT_MIRROR_DIR=str(mirror), AUDIT_MIRROR_SECONDS='5', AUDIT_VERIFY_SECONDS='5')
    proc = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ok = True
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{PORT}/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())['CG-4004']
        op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        req = urllib.request.Request(f'http://127.0.0.1:{PORT}/api/v1/auth/login', json.dumps({'badge_id': 'CG-4004', 'password': acc['password'], 'totp': pyotp.TOTP(acc['secret']).now()}).encode(),
                                     {'content-type': 'application/json'})
        op.open(req, timeout=10)

        def guard():
            s, h = api(op, '/api/v1/ops/health')
            return next(c for c in h['components'] if c['id'] == 'audit_guard')

        for _ in range(40):
            g = guard()
            if g['status'] == 'ok':
                break
            time.sleep(1)
        print('before the edit :', g['status'], '-', g['detail'][:110])
        ok &= g['status'] == 'ok'
        log = data / 'security_audit.jsonl'
        before = log.read_bytes()
        lines = before.split(b'\n')
        lines[2] = lines[2].replace(b'"login_success"', b'"login_failure"', 1) if b'"login_success"' in lines[2] else lines[2][:-3] + b'xx}'
        log.write_bytes(b'\n'.join(lines))
        for _ in range(40):
            g = guard()
            if g['status'] == 'down':
                break
            time.sleep(1)
        print('after the edit  :', g['status'], '-', g['detail'][:200])
        ok &= g['status'] == 'down' and ('differ' in g['detail'] or 'verify' in g['detail'] or 'does not' in g['detail'])
        mirrored = (mirror / 'security_audit.jsonl').read_bytes()
        same = before.startswith(mirrored) or mirrored.startswith(before[:len(mirrored)])
        print('mirror still the original prefix:', same)
        ok &= same and mirrored != log.read_bytes()[:len(mirrored)]
    finally:
        proc.kill()
    print('AUDIT PROTECTION NOTICED THE REWRITE' if ok else 'FAILED')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
