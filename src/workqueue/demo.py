"""A self-contained demo: sample accounts, sample claims and an in-process worker loop, so the console runs with no Redis, no Celery
worker and no database. It is for local demos only (the dev flag of the server and the desktop app's local mode). Production never
imports this module: there the workers are the Celery processes and the accounts come from the administrator.

The accounts get generated passwords and authenticator seeds that live only in memory and in the dictionary returned to the caller.
"""
import copy
import re
import secrets
import threading
import time
from datetime import date, timedelta
from urllib.parse import parse_qs, urlparse

import pyotp
from access import passwords

from . import reconcile as reconcile_mod
from . import relay, routing_config as rc
from .jobs import run_job
from .tasks import make_app, publisher

# Synthetic employees, one or two per clearance level, so the demo shows each level's own console (docs/62).
DEMO_ACCOUNTS = ((1, 'CG-1001', 'Nadia Haddad'), (2, 'CG-2002', 'Amal Bennani'), (2, 'CG-2003', 'Omar Saidi'),
                 (3, 'CG-3003', 'Layla Mansour'), (3, 'CG-3004', 'Karim Zaidi'), (4, 'CG-4004', 'Sami Khoury'))
DEMO_TITLES = {1: 'Claims analyst (read-only)', 2: 'Claims officer', 3: 'Senior claims officer', 4: 'Claims operations supervisor'}


def _password(badge):
    while True:
        candidate = secrets.token_urlsafe(14) + 'aA1!'
        if not passwords.check_policy(candidate, badge):
            return candidate


def seed_accounts(stack):
    """Create the demo accounts. Returns {badge: {level, name, password, secret}}."""
    out = {}
    for level, badge, name in DEMO_ACCOUNTS:
        password = _password(badge)
        _, uri = stack.service.provision_user(badge, name, password, level, created_by='demo', must_change_password=False)
        out[badge] = {'level': level, 'name': name, 'title': DEMO_TITLES[level], 'password': password, 'secret': parse_qs(urlparse(uri).query)['secret'][0]}
    return out


def current_code(secret):
    return pyotp.TOTP(secret).now()


def configure_shift(queue, badges, slice_size=6, low_water=3):
    """Put these badges on shift in a new routing configuration version."""
    import dataclasses
    store = queue.store
    latest = store.latest_config()
    base = rc.from_doc(latest) if latest else rc.DEFAULT
    cfg = rc.validate(dataclasses.replace(base, version=(latest['version'] if latest else 0) + 1, on_shift=tuple(badges),
                                          slice_size=slice_size, low_water=low_water))
    if not store.put_config(rc.to_doc(cfg), latest['version'] if latest else 0):
        raise RuntimeError('the routing configuration changed while the demo was starting')
    return cfg


def load_claims(queue, claims, limit=60):
    """Submit up to `limit` claims through the real intake. Returns how many were accepted."""
    accepted = 0
    for claim in claims:
        if accepted >= limit:
            break
        try:
            queue.intake.submit(claim)
        except ValueError:
            continue
        accepted += 1
    return accepted


class WorkerLoop:
    """Does what the Celery worker and its scheduler do, on a thread: publish the outbox (which runs the pipeline eagerly), deal claims
    into inboxes, return lapsed leases. The same functions, the same stores; only the clock tick is different."""

    def __init__(self, queue, relay_every=1.5, deal_every=2.0, expire_every=20.0, reconcile_every=120.0):
        self.queue = queue
        self.app = make_app('memory://', eager=True, runtime=queue.runtime)
        self.every = {'relay': relay_every, 'deal': deal_every, 'expire': expire_every, 'reconcile': reconcile_every}
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, name='claimguard-demo-worker', daemon=True)
        self.errors = 0

    def start(self):
        self.thread.start()
        return self

    def tick(self, name):
        rt = self.queue.runtime
        if name == 'relay':
            run_job(rt.store, rt.clock, 'relay', lambda: relay.sweep(rt.store, publisher(self.app), rt.clock()))
        elif name == 'deal':
            run_job(rt.store, rt.clock, 'deal', lambda: len(rt.dispatcher.deal(full=False).assigned))
        elif name == 'expire':
            run_job(rt.store, rt.clock, 'expire', rt.dispatcher.expire)
        else:
            run_job(rt.store, rt.clock, 'reconcile', lambda: reconcile_mod.reconcile(rt.store, rt.clock()))

    def _run(self):
        last = {k: 0.0 for k in self.every}
        while not self.stop.wait(0.25):
            now = time.monotonic()
            for name, gap in self.every.items():
                if now - last[name] >= gap:
                    last[name] = now
                    try:
                        self.tick(name)
                    except Exception:  # noqa: BLE001 - a demo loop keeps going; the real workers have retries and dead letters
                        self.errors += 1

    def close(self):
        self.stop.set()
        self.thread.join(timeout=3)


HISTORY_DONORS = 60      # the last claims of the sample become earlier visits of returning patients (never fed to "Simulate new claims")


def _shift(day, days):
    return (date.fromisoformat(day) - timedelta(days=days)).isoformat() if isinstance(day, str) else day


def earlier_visit(claim, patient_of, months_back):
    """A synthetic earlier claim of the same patient: `claim` re-addressed to the patient of `patient_of`, with its submission and service dates
    moved back so it lands `months_back` months before that claim. Everything else (services, amounts, provider) is the donor's own."""
    visit = copy.deepcopy(claim)
    target = date.fromisoformat(patient_of['submission_date']) - timedelta(days=30 * months_back)
    days = (date.fromisoformat(visit['submission_date']) - target).days
    visit['submission_date'] = target.isoformat()
    for line in visit.get('lines') or []:
        line['service_date'] = _shift(line.get('service_date'), days)
    visit['patient_id'], visit['member_id'] = patient_of['patient_id'], patient_of['member_id']
    for doc in visit.get('attachments') or []:              # the documents belong to the same patient and move with the visit
        if isinstance(doc, dict):
            doc['patient_id'] = patient_of['patient_id']
            doc['service_date'] = _shift(doc.get('service_date'), days)
            if isinstance(doc.get('text'), str) and isinstance(doc['service_date'], str):
                doc['text'] = re.sub(r'\d{4}-\d{2}-\d{2}', doc['service_date'], doc['text'])
    cov = visit.get('coverage')
    if isinstance(cov, dict):
        cov['beneficiary_patient_id'], cov['member_id'] = patient_of['patient_id'], patient_of['member_id']
    return visit


def seed_returning_patients(queue, loaded, donors):
    """Give every third loaded claim's patient two or three earlier claims, so "Show this patient's other claims" (and the cross-claim advisory
    checks) have something to show in the demo: the sample itself has one claim per patient. Returns how many visits were accepted."""
    donors, accepted = list(donors), 0
    for i, claim in enumerate(loaded[::3]):
        for months in range(1, 2 + i % 2 + 1):
            if not donors:
                return accepted
            try:
                queue.intake.submit(earlier_visit(donors.pop(), claim, months))
                accepted += 1
            except (ValueError, KeyError):
                continue
    return accepted


def start_demo(stack, claims, limit=60):
    """Seed accounts, put the reviewers on shift, load claims, give some patients earlier claims and start the worker loop. Returns (accounts, loop)."""
    accounts = seed_accounts(stack)
    configure_shift(stack.queue, [b for b, a in accounts.items() if a['level'] in (2, 3)])
    load_claims(stack.queue, claims, limit)
    seed_returning_patients(stack.queue, claims[:limit], claims[-HISTORY_DONORS:])
    return accounts, WorkerLoop(stack.queue).start()


class ArrivalFeed:
    """The demo's intake log, for the console's notification bell (GET /demo/feed) and its "Simulate new claims" button
    (POST /demo/arrivals). It wraps the intake's submit, so every new claim counts whichever way it came in: the button, the
    supervisor's Submit page or a file import. A resubmission of a claim already stored (same version) is not news and is not logged.
    The pool is the sample claims the demo did not load at start."""

    def __init__(self, queue, pool, keep=40):
        self.queue, self.pool, self.keep = queue, list(pool), keep
        self.lock = threading.Lock()
        self.seq, self.recent = 0, []
        original = queue.intake.submit

        def submit(claim):
            claim_id = claim.get('claim_id') if isinstance(claim, dict) else None
            before = self._version(claim_id)
            receipt = original(claim)
            after = self._version(claim_id)
            if after is not None and after != before:
                self._log(claim_id, receipt.get('lane'))
            return receipt

        queue.intake.submit = submit

    def _version(self, claim_id):
        try:
            doc = self.queue.store.get(claim_id) if isinstance(claim_id, str) else None
        except ValueError:
            return None
        return doc['version'] if doc else None

    def _log(self, claim_id, lane):
        with self.lock:
            self.seq += 1
            self.recent.append({'seq': self.seq, 'claim_id': claim_id, 'lane': lane, 'at': time.time()})
            del self.recent[:-self.keep]

    def snapshot(self, last=20):
        with self.lock:
            return {'seq': self.seq, 'recent': [dict(r) for r in self.recent[-last:]]}

    def arrive(self, n):
        """Send the next n sample claims through the real intake. Returns how many were accepted."""
        accepted = 0
        while accepted < n:
            with self.lock:
                if not self.pool:
                    break
                claim = self.pool.pop(0)
            try:
                self.queue.intake.submit(claim)
            except ValueError:
                continue
            accepted += 1
        return accepted
