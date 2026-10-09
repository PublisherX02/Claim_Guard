"""API conformance: the same requests, as the same roles, against the Python server and the Rust server (both in --demo mode).

Usage: python rust/tools/conformance.py   (starts both servers itself on ports 8461/8462 and stops them)

Each server generates its own demo accounts, so requests are made per role. Responses are compared by status code and by JSON
*shape* (keys and value types, lists by their first element), with volatile values (times, ids, tokens, hashes, counts) ignored, and
exact values for the fixed-vocabulary fields (error codes, statuses, review states, allowed actions, permissions).
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import http.cookiejar
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[2]
PY_PORT, RS_PORT = 8461, 8462
EXACT = {'error', 'status', 'review_state', 'allowed_actions', 'permissions', 'level', 'rule_id', 'severity', 'max_severity', 'flagged',
         'lane', 'state', 'ok', 'overall', 'detail_code', 'must_change_password', 'recorded', 'action'}


def start(kind, port, data):
    env = dict(os.environ, CLAIMGUARD_DATA_DIR=str(data), PYTHONUTF8='1')
    if kind == 'py':
        cmd = [sys.executable, str(ROOT / 'scripts' / 'serve_access.py'), '--demo', '--port', str(port)]
    else:
        cmd = [str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'), 'serve', '--demo', '--port', str(port), '--data-dir', str(data)]
    p = subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(120):
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=1)
            return p
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    p.kill()
    raise SystemExit(f'{kind} server did not start')


class Client:
    def __init__(self, port, accounts):
        self.base, self.acc, self.sessions = f'http://127.0.0.1:{port}', accounts, {}

    def raw(self, op, method, path, body=None, csrf=None, headers=None):
        h = {'content-type': 'application/json'}
        if csrf:
            h['x-csrf-token'] = csrf
        h.update(headers or {})
        data = json.dumps(body).encode() if body is not None else None
        try:
            r = op.open(urllib.request.Request(self.base + path, data, h, method=method), timeout=15)
            status, text = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, text = e.code, e.read()
        except (TimeoutError, OSError):
            status, text = 'TIMEOUT', b'null'
        try:
            return status, json.loads(text)
        except ValueError:
            return status, text[:60]

    def login(self, badge):
        if badge in self.sessions:
            return self.sessions[badge]
        a = self.acc[badge]
        op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        while True:
            status, body = self.raw(op, 'POST', '/api/v1/auth/login', {'badge_id': badge, 'password': a['password'], 'totp': pyotp.TOTP(a['secret']).now()})
            if status == 200:
                break
            tries = locals().get('tries', 0) + 1
            if tries > 40:
                raise SystemExit(f'login failed for {badge} on {self.base}: {status} {body}')
            time.sleep(1)  # the same one-time code cannot be used twice in a step
        self.sessions[badge] = (op, body['csrf'])
        return self.sessions[badge]

    def call(self, badge, method, path, body=None):
        if badge is None:
            return self.raw(urllib.request.build_opener(), method, path, body)
        op, csrf = self.login(badge)
        return self.raw(op, method, path, body, csrf if method not in ('GET', 'HEAD') else None)


def shape(v, key=''):
    if isinstance(v, dict):
        return {k: shape(x, k) for k, x in sorted(v.items())}
    if isinstance(v, list):
        if key in EXACT:
            return sorted(map(str, v))
        return [shape(v[0], key)] if v else []
    if key in EXACT:
        return v
    return type(v).__name__ if not isinstance(v, (int, float)) or isinstance(v, bool) else 'number'


def main():
    base = Path(os.environ.get('TEMP', '/tmp')) / 'cg_conf'
    (base / 'py').mkdir(parents=True, exist_ok=True)
    (base / 'rs').mkdir(parents=True, exist_ok=True)
    procs = [start('py', PY_PORT, base / 'py'), start('rs', RS_PORT, base / 'rs')]
    try:
        clients = [Client(p, json.loads((base / d / 'demo_accounts.json').read_text())) for p, d in ((PY_PORT, 'py'), (RS_PORT, 'rs'))]
        time.sleep(4)  # let both workers deal the claims
        ids = [c['claim_id'] for c in clients[0].call('CG-3003', 'GET', '/api/v1/claims?limit=200')[1]['claims']]
        sample = ids[:6]
        reqs = [(None, 'GET', '/healthz', None), (None, 'GET', '/api/v1/claims', None), (None, 'GET', '/api/v1/nope', None), (None, 'POST', '/api/v1/auth/login', {'badge_id': 'x'})]
        for badge in ('CG-1001', 'CG-2002', 'CG-3003', 'CG-4004'):
            reqs += [(badge, 'GET', '/api/v1/auth/me', None)]
            for p in ('/api/v1/claims?limit=3', '/api/v1/claims?limit=0', '/api/v1/claims?status=FAIL&limit=5', '/api/v1/claims?rule_id=R9', f'/api/v1/claims/{sample[0]}', f'/api/v1/claims/{sample[1]}',
                      '/api/v1/claims/does-not-exist', '/api/v1/audit/events?limit=2', '/api/v1/audit/events?event_type=login_success&newest_first=true&limit=2', '/api/v1/audit/events?event_type=zzz',
                      '/api/v1/audit/verify', '/api/v1/ops/audit/summary?hours=24', '/api/v1/ops/health', '/api/v1/users', '/api/v1/queue/dashboard', '/api/v1/queue/feedback', '/api/v1/queue/config',
                      '/api/v1/work/inbox', f'/api/v1/ops/trace/{sample[0]}', '/ui/', '/ui/app.js', '/ui/nothing.js'):
                reqs.append((badge, 'GET', p, None))
            reqs += [(badge, 'POST', f'/api/v1/claims/{sample[2]}/unmask', {'reason': 'conformance'}), (badge, 'POST', f'/api/v1/claims/{sample[2]}/unmask', {}),
                     (badge, 'POST', f'/api/v1/claims/{sample[3]}/findings/R001/decision', {'action': 'confirm_issue', 'reason': 'ok'}),
                     (badge, 'POST', f'/api/v1/claims/{sample[3]}/findings/R001/decision', {'action': 'bad', 'reason': 'ok'}),
                     (badge, 'POST', '/api/v1/work/heartbeat', {}), (badge, 'POST', '/api/v1/work/next', {}),
                     (badge, 'POST', '/api/v1/users', {'badge_id': 'CG-7007', 'name': 'T', 'password': 'Correct-Horse-9!x', 'level': 2}),
                     (badge, 'PATCH', '/api/v1/queue/config', {'expected_version': 999, 'slice_size': 6}),
                     (badge, 'POST', '/api/v1/auth/change-password', {'old_password': 'x', 'new_password': 'y'})]
        bad = 0
        for badge, m, path, body in reqs:
            print(badge, m, path, flush=True)
            (s1, b1), (s2, b2) = [c.call(badge, m, path, body) for c in clients]
            if path in ('/ui/', '/ui/app.js'):
                same = s1 == s2
            else:
                same = s1 == s2 and shape(b1) == shape(b2)
            if not same:
                bad += 1
                print(f'DIFF {badge} {m} {path}\n  py {s1} {json.dumps(shape(b1))[:400]}\n  rs {s2} {json.dumps(shape(b2))[:400]}')
        print(f'{len(reqs)} requests compared, {bad} differences')
        return 1 if bad else 0
    finally:
        for p in procs:
            p.kill()


if __name__ == '__main__':
    sys.exit(main())
