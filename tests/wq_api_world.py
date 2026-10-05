"""A world for the queue service and its HTTP routes: the access world (users, sessions, security log) plus a queue store, a dispatcher
fed from the live user store, the real engine results of the 50 stress claims and logged-in test clients.

Not a test module (no test_ prefix).
"""
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import api
from access_api_world import shared_claim_store
from access_world import BADGES, PASSWORDS, World
from audit_log import AuditLog
from fastapi.testclient import TestClient
from workqueue import pipeline, routing_config as rc
from workqueue.dispatcher import Dispatcher
from workqueue.intake import Intake
from workqueue.service import QueueService, agents_from_access

EXTRA = {'CG-3004': 3, 'CG-3005': 3, 'CG-2003': 2, 'CG-2004': 2}


class QueueWorld(World):
    def __init__(self, qstore, **setting_overrides):
        super().__init__(**setting_overrides)
        self._review_tmp = tempfile.TemporaryDirectory()
        self.qstore = qstore
        self.claims = shared_claim_store()
        self.review_log = AuditLog(Path(self._review_tmp.name) / 'review_audit.jsonl')
        self.dispatcher = Dispatcher(qstore, agents_from_access(self.service), self.clock, rng_seed=3, securitylog=self.log)
        self.queue = QueueService(qstore, self.dispatcher, self.service, self.log, self.review_log, self.clock)
        self.intake = Intake(qstore, lambda claim: self.claims.get(claim['claim_id'])[1], self.log, self.clock, 'pack', 'engine')
        self.app = None
        self.clients = []
        self._salt = 0

    def close(self):
        for c in self.clients:
            c.close()
        self._review_tmp.cleanup()
        super().close()

    # ---- people
    def staff(self, extra=True):
        self.provision_all()
        if extra:
            for badge, level in EXTRA.items():
                self.provision(level, badge=badge, name=f'User {badge}', password=PASSWORDS[level])

    def on_shift(self, *badges, **more):
        """Write the next routing configuration with these badges on shift."""
        from wq_world import set_cfg
        return set_cfg(self.qstore, on_shift=tuple(badges), **more)

    def principal(self, level=None, badge=None):
        """A fresh session's Principal for this badge (or the seeded user of the level)."""
        badge = badge or BADGES[level]
        self.clock.advance(31)
        return self.service.authenticate(self.service.login(badge, PASSWORDS[int(self.level_of(badge))], self.code(badge), '127.0.0.1').token)

    def level_of(self, badge):
        return self.store.get_user(badge).level

    # ---- claims
    def claim_ids(self):
        return [row['claim_id'] for row in self.claims.summaries(limit=200)]

    def find(self, wanted):
        """A stress claim id by its findings: 'green', 'medium' (flagged, all medium), 'high' (some high finding)."""
        for cid in self.claim_ids():
            _, results = self.claims.get(cid)
            flagged = [r for r in results if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS')]
            kinds = {r['severity'] for r in flagged}
            if (wanted == 'green' and not flagged) or (wanted == 'medium' and flagged and kinds == {'medium'}) \
                    or (wanted == 'high' and 'high' in kinds):
                if not self.qstore.get(cid):
                    return cid
        raise LookupError(wanted)

    def ready(self, claim_id, explanation=None):
        """Submit a stress claim and walk it to `ready` (through `explained` when an explanation is given)."""
        claim, _ = self.claims.get(claim_id)
        self.intake.submit(claim)
        self.qstore.clear_outbox(claim_id, 1)
        now = self.clock()
        if explanation is not None:
            self.qstore.transition(claim_id, 1, 'triaged', 'explained', 'system:t', now, set_fields={'explanation': explanation})
            self.qstore.transition(claim_id, 1, 'explained', 'ready', 'system:t', now)
        else:
            self.qstore.transition(claim_id, 1, 'triaged', 'ready', 'system:t', now)
        return claim_id

    def lease_to(self, claim_id, badge, seconds=1800):
        return self.qstore.lease(claim_id, 1, badge, self.clock(), self.clock() + seconds)

    def decisions(self, claim_id):
        return self.qstore.get(claim_id)['decisions']

    def events(self):
        return [r['event'] for r in self.log.events(limit=1000)]

    def review_rows(self):
        path = self.review_log.path
        return [] if not path.exists() else [r for r in path.read_text(encoding='utf-8').split('\n') if r.strip()]

    # ---- HTTP
    def build_app(self):
        self.app = api.create_app(self.service, self.claims, self.review_log, self.log, self.settings, clock=self.clock, queue=self.queue)
        return self.app

    def client(self):
        c = TestClient(self.app or self.build_app())
        self.clients.append(c)
        return c

    def login_client(self, level=None, badge=None):
        badge = badge or BADGES[level]
        password = PASSWORDS[self.level_of(badge)]
        self.clock.advance(31)
        c = self.client()
        r = c.post('/api/v1/auth/login', json={'badge_id': badge, 'password': password, 'totp': self.code(badge)})
        assert r.status_code == 200, r.text
        c.csrf = r.json()['csrf']
        c.headers.update({'X-CSRF-Token': c.csrf})
        return c
