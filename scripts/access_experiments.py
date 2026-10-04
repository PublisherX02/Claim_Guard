"""Evidence for the reviewer API, produced against a real server over real HTTP.

    python scripts/access_experiments.py smoke          # a scripted tour of the whole API (every claim checked, pass or fail)
    python scripts/access_experiments.py load           # login and read latency at 1, 8 and 32 concurrent clients
    python scripts/access_experiments.py all --out outputs/defense/access.json

The server is uvicorn running in this process; the store is MongoDB when MONGO_URI is set (a throwaway database that is dropped
afterwards) and in-memory otherwise, and the result says which. The load run uses the production password cost (bcrypt 12), because
hashing dominates login time. Latency depends on the machine; the environment is recorded beside the numbers.
"""
import argparse
import json
import os
import platform
import re
import sys
import tempfile
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
import httpx
import pyotp
import uvicorn
from access import bootstrap, passwords, totp
from access.store import MemoryStore, User
from audit_log import ANCHOR_KEY_ENV
from eval_metrics import percentile
from eval_sets import read_git_head

API = '/api/v1'
SCALES = {
    'full': {'login': {1: 24, 8: 64, 32: 96}, 'read': {1: 200, 8: 400, 32: 800}},
    'tiny': {'login': {1: 2, 4: 4}, 'read': {1: 6, 4: 12}},
}
DEMO = ((1, 'CG-1001'), (2, 'CG-2002'), (3, 'CG-3003'), (4, 'CG-4004'))
PASSWORDS = {1: 'Viewer-Pass-1357!', 2: 'Reviewer-Pass-2468!', 3: 'Senior-Pass-3579!', 4: 'Admin-Pass-4680!'}


class Checks:
    def __init__(self):
        self.items = []

    def check(self, name, ok, detail=''):
        item = {'name': name, 'ok': bool(ok)}
        if not ok and detail:
            item['detail'] = str(detail)[:200]
        self.items.append(item)
        print(('PASS ' if ok else 'FAIL ') + name, file=sys.stderr)

    def summary(self):
        return {'checks': self.items, 'passed': sum(c['ok'] for c in self.items), 'total': len(self.items)}


class Server:
    """The application under a real uvicorn server on a free localhost port, with users in the given store."""

    def __init__(self, env, store=None):
        self._tmp = tempfile.TemporaryDirectory()
        os.environ.pop(ANCHOR_KEY_ENV, None)                       # one anchor key per process; each server brings its own
        self.app, self.stack = bootstrap.build_app({**env}, dev=True, store=store, data_dir=self._tmp.name,
                                                   claims_path=ROOT / 'data' / 'stress' / 'claims.jsonl')
        self.server = uvicorn.Server(uvicorn.Config(self.app, host='127.0.0.1', port=0, log_level='warning', server_header=False))
        self.thread = threading.Thread(target=self.server.run, daemon=True)

    def __enter__(self):
        self.thread.start()
        deadline = time.time() + 20
        while not self.server.started:
            if time.time() > deadline or not self.thread.is_alive():
                raise RuntimeError('the server did not start')
            time.sleep(0.05)
        self.port = self.server.servers[0].sockets[0].getsockname()[1]
        return self

    def __exit__(self, *exc):
        self.server.should_exit = True
        self.thread.join(timeout=10)
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self._tmp.cleanup()

    @property
    def base_url(self):
        return f'http://127.0.0.1:{self.port}'

    def provision_levels(self):
        users = {}
        for level, badge in DEMO:
            _, uri = self.stack.service.provision_user(badge, f'Smoke L{level}', PASSWORDS[level], level, must_change_password=False)
            users[level] = (badge, PASSWORDS[level], parse_qs(urlparse(uri).query)['secret'][0])
        return users

    def bulk_users(self, n, level=2, password='Load-Test-Pass-2468!'):
        """n users sharing one password hash and one authenticator seed (codes are single-use per badge, so sharing a seed is safe)."""
        settings = self.stack.settings
        hashed = passwords.hash_password(password, settings.bcrypt_rounds)
        secret = totp.new_secret()
        enc = totp.encrypt_secret(secret, settings.fernet_key)
        badges = [f'CG-{600000 + i}' for i in range(n)]
        for badge in badges:
            self.stack.store.create_user(User(badge_id=badge, name='Load user', password_hash=hashed, totp_secret_enc=enc, level=level,
                                              created_by='experiment', created_at=time.time()))
        return badges, password, secret


def _env(rounds):
    return {'BCRYPT_ROUNDS': str(rounds), 'COOKIE_SECURE': 'false', 'TOKEN_TTL_SECONDS': '86400'}


def _login(client, badge, password, secret, code=None):
    r = client.post(f'{API}/auth/login', json={'badge_id': badge, 'password': password, 'totp': code or pyotp.TOTP(secret).now()})
    if r.status_code == 200:
        client.headers['X-CSRF-Token'] = r.json()['csrf']
    return r


def smoke(rounds=12, store=None):
    checks = Checks()
    with Server(_env(rounds), store=store) as srv:
        users = srv.provision_levels()
        base = srv.base_url
        client = lambda: httpx.Client(base_url=base, timeout=30)          # noqa: E731
        checks.check('health needs no login', httpx.get(base + '/healthz').json() == {'status': 'ok'})
        checks.check('claims need a login', httpx.get(base + f'{API}/claims').status_code == 401)
        r = _login(client(), users[2][0], 'Wrong-Pass-1357!', users[2][2])
        checks.check('a wrong password gets the generic refusal', r.status_code == 401 and r.json() == {'error': 'invalid_credentials'}, r.text)

        viewer = client()
        viewer_code = pyotp.TOTP(users[1][2]).now()
        r = _login(viewer, *users[1], code=viewer_code)
        checks.check('a viewer logs in with badge, password and authenticator code', r.status_code == 200, r.text)
        me = viewer.get(f'{API}/auth/me').json()
        checks.check('the viewer is level 1 and cannot decide claims', me['level'] == 1 and 'claims.decide' not in me['permissions'], me)
        listing = viewer.get(f'{API}/claims', params={'limit': 5})
        checks.check('the viewer can list claims', listing.status_code == 200 and len(listing.json()['claims']) == 5, listing.text[:100])
        cid = listing.json()['claims'][0]['claim_id']
        detail = viewer.get(f'{API}/claims/{cid}')
        checks.check('the viewer sees no raw patient or member identifier',
                     re.search(r'(?:PAT|MEM)-[0-9A-F]{10}\b', detail.text) is None and re.search(r'(?:PAT|MEM)-[0-9A-F]{8}\b', detail.text) is not None)
        checks.check('the viewer sees no notes', 'notes' not in detail.json()['claim'])
        checks.check('the viewer cannot unmask', viewer.post(f'{API}/claims/{cid}/unmask', json={'reason': 'curiosity'}).status_code == 403)
        checks.check('the viewer cannot read the audit log', viewer.get(f'{API}/audit/events').status_code == 403)

        reviewer = client()
        r = _login(reviewer, *users[2])
        checks.check('a reviewer logs in', r.status_code == 200, r.text)
        checks.check('the reviewer sees the notes field', 'notes' in reviewer.get(f'{API}/claims/{cid}').json()['claim'])
        medium = high = None
        for row in reviewer.get(f'{API}/claims', params={'limit': 200}).json()['claims']:
            for f in reviewer.get(f"{API}/claims/{row['claim_id']}").json()['findings']:
                if f['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
                    if f['severity'] == 'medium' and not medium:
                        medium = (row['claim_id'], f['rule_id'])
                    if f['severity'] == 'high' and not high:
                        high = (row['claim_id'], f['rule_id'])
            if medium and high:
                break
        body = {'action': 'confirm_issue', 'reason': 'Smoke check'}
        r = reviewer.post(f'{API}/claims/{high[0]}/findings/{high[1]}/decision', json=body)
        checks.check('the reviewer is refused on a high-severity finding', r.status_code == 403, r.text)
        r = reviewer.post(f'{API}/claims/{medium[0]}/findings/{medium[1]}/decision', json=body)
        checks.check('the reviewer decides a medium-severity finding', r.status_code == 200, r.text)
        r = reviewer.post(f'{API}/claims/{medium[0]}/findings/{medium[1]}/decision', json={**body, 'actor': 'CG-3003'})
        checks.check('the reviewer cannot choose the recorded actor', r.status_code == 422, r.text)
        r = reviewer.post(f'{API}/claims/{cid}/unmask', json={'reason': 'Calling the member'})
        checks.check('the reviewer can unmask with a reason', r.status_code == 200 and re.search(r'(?:PAT|MEM)-[0-9A-F]{10}\b', r.text) is not None)

        senior = client()
        r = _login(senior, *users[3])
        checks.check('a senior reviewer logs in', r.status_code == 200, r.text)
        r = senior.post(f'{API}/claims/{high[0]}/findings/{high[1]}/decision', json=body)
        checks.check('the senior decides the high-severity finding', r.status_code == 200, r.text)

        admin = client()
        r = _login(admin, *users[4])
        checks.check('an administrator logs in', r.status_code == 200, r.text)
        checks.check('the administrator cannot read claims (separation of duties)', admin.get(f'{API}/claims').status_code == 403)
        events = admin.get(f'{API}/audit/events', params={'limit': 1000}).json()['events']
        kinds = {e['event']['event_type'] for e in events}
        checks.check('the audit log shows logins, failures, refusals, decisions and unmasking',
                     {'login_success', 'login_failure', 'forbidden', 'decision', 'unmask'} <= kinds, sorted(kinds))
        recorded = {e['event']['badge_id'] for e in events if e['event']['event_type'] == 'decision'}
        checks.check('decisions are recorded under the logged-in badges', recorded == {users[2][0], users[3][0]}, recorded)
        v = admin.get(f'{API}/audit/verify').json()
        checks.check('both audit chains verify and the security anchor is signed', v['security']['ok'] and v['security']['anchor_signed'] and v['review']['ok'], v)
        dump = json.dumps(events)
        checks.check('no password, code or seed appears in the audit log',
                     not any(secret in dump for _, _, secret in users.values()) and not any(pw in dump for _, pw, _ in users.values()))
        replay = httpx.post(base + f'{API}/auth/login', json={'badge_id': users[1][0], 'password': users[1][1], 'totp': viewer_code})
        checks.check('a one-time code that already logged someone in is refused as a replay', replay.status_code == 401)
    return checks.summary()


def _summarise(latencies, errors, wall):
    ms = [t * 1000 for t in latencies]
    return {'n': len(latencies) + errors, 'errors': errors, 'p50_ms': round(percentile(ms, 50), 2), 'p95_ms': round(percentile(ms, 95), 2),
            'p99_ms': round(percentile(ms, 99), 2), 'max_ms': round(max(ms), 2), 'throughput_per_s': round((len(latencies) + errors) / wall, 2)}


def _run(concurrency, n, task):
    latencies, errors, lock = [], [0], threading.Lock()

    def timed(i):
        start = time.perf_counter()
        ok = task(i)
        took = time.perf_counter() - start
        with lock:
            if ok:
                latencies.append(took)
            else:
                errors[0] += 1
    wall = time.perf_counter()
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        list(pool.map(timed, range(n)))
    return _summarise(latencies or [0.0], errors[0], time.perf_counter() - wall)


def load(rounds=12, plan=None, store=None):
    plan = plan or SCALES['full']
    result = {'bcrypt_rounds': rounds, 'login': {}, 'read': {}}
    with Server(_env(rounds), store=store) as srv:
        base = srv.base_url
        need = sum(plan['login'].values()) + 1
        badges, password, secret = srv.bulk_users(need)
        cursor = 0
        for concurrency, n in plan['login'].items():
            batch = badges[cursor:cursor + n]
            cursor += n

            def log_in(i, batch=batch):
                with httpx.Client(base_url=base, timeout=60) as c:
                    return _login(c, batch[i], password, secret).status_code == 200
            result['login'][str(concurrency)] = _run(concurrency, n, log_in)
        session = httpx.Client(base_url=base, timeout=60)
        _login(session, badges[cursor], password, secret)
        token = session.cookies.get('cg_session')
        claim_ids = [row['claim_id'] for row in session.get(f'{API}/claims', params={'limit': 200}).json()['claims']]
        local = threading.local()

        def read(i):
            if not hasattr(local, 'client'):
                local.client = httpx.Client(base_url=base, timeout=60, cookies={'cg_session': token})
            return local.client.get(f'{API}/claims/{claim_ids[i % len(claim_ids)]}').status_code == 200
        for concurrency, n in plan['read'].items():
            result['read'][str(concurrency)] = _run(concurrency, n, read)
        session.close()
    return result


def _environment():
    return {'commit': read_git_head(ROOT), 'platform': platform.platform(), 'python': platform.python_version(), 'cpus': os.cpu_count(), 'uvicorn': uvicorn.__version__,
            'note': 'server and clients share one machine and one process; latency is machine-dependent'}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('command', choices=('smoke', 'load', 'all'))
    ap.add_argument('--scale', choices=sorted(SCALES), default='full')
    ap.add_argument('--rounds', type=int, default=12)
    ap.add_argument('--out', default='outputs/defense/access.json')
    args = ap.parse_args(argv)
    uri = os.environ.get('MONGO_URI')
    store, mongo, label = None, None, 'memory'
    if uri:
        from access.store_mongo import MongoStore
        mongo = MongoStore(uri, 'claimguard_exp_' + uuid.uuid4().hex[:10])
        mongo.ensure_indexes()
        store, label = mongo, 'mongodb ' + mongo._client.server_info()['version']
    out = {'environment': _environment(), 'store': label}
    try:
        if args.command in ('smoke', 'all'):
            out['smoke'] = smoke(args.rounds, store)
        if args.command in ('load', 'all'):
            out['load'] = load(args.rounds, SCALES[args.scale], store)
    finally:
        if mongo:
            mongo.drop_database()
            mongo.close()
    path = Path(args.out)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, indent=2), encoding='utf-8')
    print(json.dumps({k: v for k, v in out.items() if k != 'smoke'}, indent=2))
    if 'smoke' in out:
        print(f"smoke: {out['smoke']['passed']} of {out['smoke']['total']} checks passed")
    smoke_result = out.get('smoke')
    return 0 if smoke_result is None or smoke_result['passed'] == smoke_result['total'] else 1

if __name__ == '__main__':
    sys.exit(main())
