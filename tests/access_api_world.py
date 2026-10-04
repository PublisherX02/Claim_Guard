"""The API test world: a World plus an app, a claim store over data/stress, a review audit log and logged-in test clients."""
import logging
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import api, claimstore
from access_world import BADGES, PASSWORDS, World
from audit_log import AuditLog
from engine_core import config
from fastapi.testclient import TestClient

_STORE = None


def shared_claim_store():
    """The 50 stress claims with their engine results, built once per process (evaluation is the slow part)."""
    global _STORE
    if _STORE is None:
        logging.disable(logging.WARNING)
        try:
            _STORE = claimstore.FileClaimStore.from_jsonl(ROOT / 'data' / 'stress' / 'claims.jsonl', config(ROOT))
        finally:
            logging.disable(logging.NOTSET)
    return _STORE


def find_finding(store, severity, status='FAIL'):
    """(claim_id, rule_id) of the first finding with this severity and status."""
    for row in store.summaries(limit=200):
        _, results = store.get(row['claim_id'])
        for r in results:
            if r['severity'] == severity and r['status'] == status:
                return row['claim_id'], r['rule_id']
    raise LookupError((severity, status))


class ApiWorld(World):
    def __init__(self, **setting_overrides):
        super().__init__(**setting_overrides)
        self._review_tmp = tempfile.TemporaryDirectory()
        self.claims = shared_claim_store()
        self.review_log = AuditLog(Path(self._review_tmp.name) / 'review_audit.jsonl')
        self.app = api.create_app(self.service, self.claims, self.review_log, self.log, self.settings, clock=self.clock)
        self.clients = []

    def close(self):
        for c in self.clients:
            c.close()
        self._review_tmp.cleanup()
        super().close()

    def client(self):
        c = TestClient(self.app)
        self.clients.append(c)
        return c

    def login_client(self, level, badge=None, password=None):
        """A client logged in as the seeded user of this level; its CSRF token is on client.csrf."""
        badge = badge or BADGES[level]
        self.clock.advance(31)
        c = self.client()
        r = c.post('/api/v1/auth/login', json={'badge_id': badge, 'password': password or PASSWORDS[level], 'totp': self.code(badge)})
        assert r.status_code == 200, r.text
        c.csrf = r.json()['csrf']
        c.headers.update({'X-CSRF-Token': c.csrf})
        return c

    def high(self):
        return find_finding(self.claims, 'high')

    def medium(self):
        return find_finding(self.claims, 'medium')
