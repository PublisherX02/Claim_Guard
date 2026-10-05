"""The queue wired for real: real engine, real masking, the HTTP routes switched on with queue=True, and the pipeline end to end."""
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import pyotp
from access import bootstrap as access_bootstrap
from access.store import MemoryStore
from audit_log import ANCHOR_KEY_ENV
from engine_core import load_jsonl
from fastapi.testclient import TestClient
from workqueue import pipeline
from workqueue.store import MemoryQueueStore
from wq_world import set_cfg

API = '/api/v1'
ENV = {'BCRYPT_ROUNDS': '4', 'COOKIE_SECURE': 'false'}
PASSWORD = 'Reviewer-Pass-2468!'


class RealWiringTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.users, self.queue_store = MemoryStore(), MemoryQueueStore()

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def build(self, queue):
        claims = Path(self.tmp.name) / 'claims.jsonl'
        lines = (ROOT / 'data' / 'development' / 'claims.jsonl').read_text(encoding='utf-8').split('\n')[:12]
        claims.write_text('\n'.join(lines) + '\n', encoding='utf-8')
        return access_bootstrap.build_app(dict(ENV), dev=True, store=self.users, data_dir=self.tmp.name, claims_path=claims, queue=queue,
                                          queue_store=self.queue_store)

    def test_queue_routes_exist_only_when_the_queue_is_switched_on(self):
        app, _ = self.build(False)
        self.assertEqual(TestClient(app).get(f'{API}/work/inbox').status_code, 404)
        app, stack = self.build(True)
        self.assertEqual(TestClient(app).get(f'{API}/work/inbox').status_code, 401)
        self.assertIsNotNone(stack.queue)

    def test_from_a_real_claim_through_the_pipeline_the_dealer_and_the_http_inbox_to_a_decision(self):
        app, stack = self.build(True)
        q = stack.queue
        _, uri = stack.service.provision_user('CG-3003', 'Senior', PASSWORD, 3, must_change_password=False)
        secret = parse_qs(urlparse(uri).query)['secret'][0]
        set_cfg(self.queue_store, on_shift=('CG-3003',), slice_size=10, low_water=2)
        rows = load_jsonl(str(ROOT / 'data' / 'development' / 'claims.jsonl'))[:12]
        for claim in rows:
            q.intake.submit(claim)
            self.assertEqual(pipeline.advance(self.queue_store, claim['claim_id'], 1, q.runtime.steps, 1.0), 'ready')
        counts = self.queue_store.counts()
        self.assertEqual(sum(counts.values()), 12)
        self.assertTrue(all(k.startswith('ready|') for k in counts))
        explained = [d for d in self.queue_store.by_state('ready', 100) if d['explanation']]
        self.assertTrue(explained)
        self.assertEqual({d['explanation']['outcome'] for d in explained}, {'skipped_no_model'})        # no model configured: the engine's text
        self.assertGreater(len(q.dispatcher.deal().assigned), 0)
        client = TestClient(app)
        r = client.post(f'{API}/auth/login', json={'badge_id': 'CG-3003', 'password': PASSWORD, 'totp': pyotp.TOTP(secret).now()})
        self.assertEqual(r.status_code, 200, r.text)
        client.headers.update({'X-CSRF-Token': r.json()['csrf']})
        got = client.get(f'{API}/work/inbox')
        self.assertEqual(got.status_code, 200, got.text)
        inbox = got.json()['claims']
        self.assertGreater(len(inbox), 0)
        self.assertEqual([p for p in {c['patient_id'] for c in rows} if p in got.text], [])        # no raw patient id in the response
        view = next((v for v in inbox if any('allowed_actions' in f for f in v['findings'])), None)
        self.assertIsNotNone(view, 'the first twelve development claims should include one with a decidable finding')
        finding = next(f for f in view['findings'] if 'allowed_actions' in f)
        self.assertTrue(finding['ai_explanation']['text'])
        ok = client.post(f"{API}/work/claims/{view['claim_id']}/findings/{finding['rule_id']}/decision",
                         json={'action': 'confirm_issue', 'reason': 'Checked against the rule text.'})
        self.assertEqual(ok.status_code, 200, ok.text)


if __name__ == '__main__':
    unittest.main()
