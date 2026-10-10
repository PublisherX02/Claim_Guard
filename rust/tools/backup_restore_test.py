"""Does a backup actually bring the system back? Brings the Rust stack up, puts data in it through the real API, takes an encrypted backup with
deploy/rust/backup.sh, destroys every volume, starts a fresh empty stack, restores with deploy/rust/restore.sh and checks that the same people
can sign in with the same passwords and authenticator seeds, the same claims and decisions are there and readable, and the audit chains verify.
Also checks the backup file itself: it is encrypted, a wrong key and a damaged file are refused, and it holds no readable claim data.

    python rust/tools/backup_restore_test.py

Needs Docker and Git Bash (for the .sh scripts) and the image claimguard-rs:latest. Uses throw-away secrets and project name cgbk.
"""
import http.cookiejar, json, os, re, secrets, shutil, ssl, subprocess, sys, tempfile, time, urllib.request, urllib.error
from pathlib import Path

import pyotp
from cryptography.fernet import Fernet

ROOT = Path(__file__).resolve().parents[2]
PROJECT = 'cgbk'
RESULTS = []
BASH = shutil.which('bash') or r'C:\Program Files\Git\bin\bash.exe'


def check(name, ok, detail=''):
    RESULTS.append(ok)
    print(('PASS ' if ok else 'FAIL ') + name + (f'  -- {detail}' if detail and not ok else ''), flush=True)


def run(args, input=None, timeout=600, env=None):
    p = subprocess.run(args, capture_output=True, text=True, encoding='utf-8', errors='replace', input=input, timeout=timeout, cwd=ROOT, env={k: v for k, v in dict(os.environ, **(env or {})).items() if k != 'MSYS_NO_PATHCONV'})
    return p.returncode, (p.stdout + p.stderr).strip()


def dc(envfile, *args, **kw):
    return run(['docker', 'compose', '-f', str(ROOT / 'deploy/rust/compose.yml'), '--env-file', str(envfile), '-p', PROJECT, *args], **kw)


def wait_healthy(envfile):
    for _ in range(80):
        rc, out = dc(envfile, 'ps', '--format', '{{.Service}} {{.Health}}')
        h = dict(l.split(' ', 1) for l in out.splitlines() if ' ' in l)
        if all(h.get(s) == 'healthy' for s in ('api', 'worker', 'mongo', 'redis')):
            return True
        time.sleep(3)
    return False


CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE


class Session:
    def __init__(self, badge, password, secret):
        self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()), urllib.request.HTTPSHandler(context=CTX))
        for _ in range(3):
            s, b = self.call('POST', '/api/v1/auth/login', {'badge_id': badge, 'password': password, 'totp': pyotp.TOTP(secret).now()})
            if s == 200:
                self.csrf = b['csrf']
                return
            time.sleep(30 - time.time() % 30 + 0.5)
        raise RuntimeError(f'cannot sign in as {badge}: {s} {b}')

    def call(self, method, path, body=None):
        h = {'content-type': 'application/json'}
        if getattr(self, 'csrf', None) and method != 'GET':
            h['x-csrf-token'] = self.csrf
        try:
            r = self.op.open(urllib.request.Request('https://localhost' + path, json.dumps(body).encode() if body is not None else None, h, method=method), timeout=30)
            return r.status, json.loads(r.read() or b'null')
        except urllib.error.HTTPError as e:
            return e.code, e.read()[:200]
        except OSError as e:
            return 0, str(e)


def snapshot(s):
    users = s.call('GET', '/api/v1/users')[1]['users']
    dash = s.call('GET', '/api/v1/queue/dashboard')[1]
    verify = s.call('GET', '/api/v1/audit/verify')[1]
    trace = s.call('GET', '/api/v1/ops/trace/CG-27BFD8541DEB')
    events = s.call('GET', '/api/v1/ops/audit/summary?hours=24')[1]
    return {'users': sorted(u['badge_id'] for u in users), 'counts': dash.get('counts'), 'verify_ok': verify['security']['ok'] and verify['review']['ok'], 'trace_status': trace[0], 'events': events.get('total')}


def main():
    tmp = Path(tempfile.mkdtemp(prefix='cg_bk_'))
    envfile, keyfile, backup = tmp / 'rs.env', tmp / 'backup.key', tmp / 'claimguard.tar.enc'
    envfile.write_text('\n'.join(['CG_SITE=localhost', 'CADDYFILE=Caddyfile.internal', 'JWT_SECRET=' + secrets.token_urlsafe(48), 'AUDIT_ANCHOR_KEY=' + secrets.token_urlsafe(48),
                                  'PII_KEY=' + secrets.token_urlsafe(48), 'FERNET_KEY=' + Fernet.generate_key().decode(), 'MONGO_ROOT_USER=claimguard',
                                  'MONGO_ROOT_PASSWORD=' + secrets.token_urlsafe(18), 'REDIS_PASSWORD=' + secrets.token_urlsafe(18)]) + '\n')
    keyfile.write_text(secrets.token_urlsafe(48) + '\n')
    senv = {'BACKUP_KEY_FILE': str(keyfile), 'COMPOSE_PROJECT': PROJECT, 'ENV_FILE': str(envfile)}
    password = 'Correct-Horse-9x!'
    try:
        rc, out = dc(envfile, 'up', '-d')
        check('stack up', rc == 0 and wait_healthy(envfile), out[-200:])
        rc, out = dc(envfile, 'run', '--rm', '-T', 'api', 'cg', 'admin', 'create-admin', '--badge', 'CG-4001', '--name', 'Backup Admin', '--password-stdin', input=password + '\n')
        m = re.search(r'secret=([A-Z2-7]+)', out)
        check('administrator created through `cg admin --password-stdin`', bool(m), out[-200:])
        secret = m.group(1)
        s = Session('CG-4001', password, secret)
        for i in range(2):
            st, b = s.call('POST', '/api/v1/users', {'badge_id': f'CG-200{i + 3}', 'name': f'Reviewer {i}', 'password': password, 'level': 2})
            assert st == 201, (st, b)
        claims = [json.loads(l) for l in (ROOT / 'data/development/claims.jsonl').read_text(encoding='utf-8').splitlines()[:12]]
        st, b = s.call('POST', '/api/v1/queue/submit', {'claims': claims})
        check('12 claims submitted through the API', st == 200 and sum(1 for r in b['results'] if r['accepted']) == 12, str(b)[:200])
        time.sleep(8)  # let the worker triage and the audit mirror run
        before = snapshot(s)
        check('before the backup: 3 users, claims in the queue, audit chains verify', len(before['users']) == 3 and before['counts'] and before['verify_ok'] and before['trace_status'] == 200, before)

        rc, out = run([BASH, 'deploy/rust/backup.sh', str(backup)], env=senv)
        check('backup.sh writes an encrypted backup and its checksum', rc == 0 and backup.exists() and (tmp / 'claimguard.tar.enc.sha256').exists(), out[-1800:])
        raw = backup.read_bytes()
        check('the backup file is encrypted (no readable claim id, badge number or JSON, not a tar or gzip)', b'CG-27BFD8541DEB' not in raw and b'CG-4001' not in raw and not raw.startswith((b'\x1f\x8b', b'ustar')) and raw.startswith(b'Salted__'), raw[:16])
        rc, out = run([BASH, 'deploy/rust/backup.sh', str(backup)], env=senv)
        check('backup.sh refuses to overwrite an existing backup', rc != 0 and 'not overwriting' in out, out[-200:])

        dc(envfile, 'down', '-v')
        rc, out = dc(envfile, 'up', '-d')
        check('a brand-new empty stack comes up (every volume destroyed)', rc == 0 and wait_healthy(envfile), out[-200:])
        try:
            Session('CG-4001', password, secret)
            check('the empty stack knows nobody', False)
        except RuntimeError:
            check('the empty stack knows nobody (sign-in refused)', True)
        # a wrong key and a damaged file are refused and change nothing
        wrongkey = tmp / 'wrong.key'
        wrongkey.write_text(secrets.token_urlsafe(48) + '\n')
        rc, out = run([BASH, 'deploy/rust/restore.sh', str(backup)], env=dict(senv, BACKUP_KEY_FILE=str(wrongkey)))
        check('restore with the wrong key is refused', rc != 0 and ('wrong key' in out or 'damaged' in out), out[-200:])
        damaged = tmp / 'damaged.tar.enc'
        damaged.write_bytes(raw[:200] + bytes([raw[200] ^ 1]) + raw[201:])
        rc, out = run([BASH, 'deploy/rust/restore.sh', str(damaged)], env=senv)
        check('restore of a damaged file is refused', rc != 0, out[-200:])
        rc, out = run([BASH, 'deploy/rust/restore.sh', str(backup)], env=senv)
        check('restore.sh restores the backup', rc == 0 and 'restored from' in out, out[-400:])
        check('the stack is healthy again after the restore', wait_healthy(envfile))
        s2 = Session('CG-4001', password, secret)
        after = snapshot(s2)
        check('the same administrator signs in with the same password and the same authenticator seed', True)
        check('the same users are there', after['users'] == before['users'], (before['users'], after['users']))
        check('the same claims are in the same states', after['counts'] == before['counts'], (before['counts'], after['counts']))
        check('claim data is readable again (the data key still opens it) and the audit chains verify', after['trace_status'] == 200 and after['verify_ok'], after)
        check('the audit history came back (at least as many events as before)', (after['events'] or 0) >= (before['events'] or 0), (before['events'], after['events']))
        # ---- rotating the data key on the restored system, with the real command
        new_key = Fernet.generate_key().decode()
        dc(envfile, 'stop', 'api', 'worker')
        rc, out = dc(envfile, 'run', '--rm', '-T', '-e', f'NEW_DATA_KEY={new_key}', 'api', 'cg', 'rotate-key', '--dry-run')
        check('cg rotate-key --dry-run counts the 12 claims and changes nothing', rc == 0 and '12 documents re-encrypted' in out and out.startswith('dry run'), out[-300:])
        rc, out = dc(envfile, 'run', '--rm', '-T', '-e', f'NEW_DATA_KEY={new_key}', 'api', 'cg', 'rotate-key')
        check('cg rotate-key re-encrypts the 12 claims', rc == 0 and '12 documents re-encrypted' in out and 'DATA_KEY' in out, out[-300:])
        rc, out = dc(envfile, 'run', '--rm', '-T', '-e', f'NEW_DATA_KEY={new_key}', 'api', 'cg', 'rotate-key')
        check('run again (as after an interruption) it finds everything already rotated', rc == 0 and '0 documents re-encrypted, 12 already on the new key' in out, out[-300:])
        envfile.write_text(envfile.read_text() + f'DATA_KEY={new_key}\n')
        dc(envfile, 'up', '-d', 'api', 'worker')
        check('the stack is healthy on the new key', wait_healthy(envfile))
        s4 = Session('CG-4001', password, secret)
        later = snapshot(s4)
        check('after the rotation the claims are in the same states, readable, and the chains verify', later['counts'] == before['counts'] and later['trace_status'] == 200 and later['verify_ok'], later)
    finally:
        dc(envfile, 'down', '-v')
    print(f'\n{len(RESULTS)} checks, {RESULTS.count(False)} FAILED')
    return 1 if False in RESULTS else 0


if __name__ == '__main__':
    sys.exit(main())
