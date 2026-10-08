"""A self-contained demo: sample accounts, sample claims and an in-process worker loop, so the console runs with no Redis, no Celery
worker and no database. It is for local demos only (the dev flag of the server and the desktop app's local mode). Production never
imports this module: there the workers are the Celery processes and the accounts come from the administrator.

The accounts get generated passwords and authenticator seeds that live only in memory and in the dictionary returned to the caller.
"""
import secrets
import threading
import time
from urllib.parse import parse_qs, urlparse

import pyotp
from access import passwords

from . import relay, routing_config as rc
from .tasks import make_app, publisher

DEMO_ACCOUNTS = ((1, 'CG-1001', 'Demo Viewer'), (2, 'CG-2002', 'Demo Reviewer Amal'), (2, 'CG-2003', 'Demo Reviewer Omar'),
                 (3, 'CG-3003', 'Demo Senior Layla'), (3, 'CG-3004', 'Demo Senior Karim'), (4, 'CG-4004', 'Demo Administrator'))


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
        out[badge] = {'level': level, 'name': name, 'password': password, 'secret': parse_qs(urlparse(uri).query)['secret'][0]}
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

    def __init__(self, queue, relay_every=1.5, deal_every=2.0, expire_every=20.0):
        self.queue = queue
        self.app = make_app('memory://', eager=True, runtime=queue.runtime)
        self.every = {'relay': relay_every, 'deal': deal_every, 'expire': expire_every}
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, name='claimguard-demo-worker', daemon=True)
        self.errors = 0

    def start(self):
        self.thread.start()
        return self

    def tick(self, name):
        rt = self.queue.runtime
        if name == 'relay':
            relay.sweep(rt.store, publisher(self.app), rt.clock())
        elif name == 'deal':
            rt.dispatcher.deal(full=False)
        else:
            rt.dispatcher.expire()

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


def start_demo(stack, claims, limit=60):
    """Seed accounts, put the reviewers on shift, load claims and start the worker loop. Returns (accounts, loop)."""
    accounts = seed_accounts(stack)
    configure_shift(stack.queue, [b for b, a in accounts.items() if a['level'] in (2, 3)])
    load_claims(stack.queue, claims, limit)
    return accounts, WorkerLoop(stack.queue).start()
