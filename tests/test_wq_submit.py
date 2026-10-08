"""POST /api/v1/queue/submit: only a level 4 operator, strict bodies, bounded batches, one bad claim never stops the rest, and nothing
is decided or assigned by the call."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from wq_api_world import QueueWorld
from wq_world import store_makers

API = '/api/v1'


class SubmitBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.w.queue.intake = self.w.intake

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def claims(self, n):
        return [self.w.claims.get(cid)[0] for cid in self.w.claim_ids()[:n]]

    def test_only_level_four_may_submit(self):
        body = {'claims': self.claims(1)}
        self.assertEqual(self.w.client().post(f'{API}/queue/submit', json=body).status_code, 401)
        for level, expected in ((1, 403), (2, 403), (3, 403), (4, 200)):
            c = self.w.login_client(level)
            self.assertEqual(c.post(f'{API}/queue/submit', json=body).status_code, expected, level)

    def test_a_submitted_claim_is_stored_triaged_with_a_receipt(self):
        c = self.w.login_client(4)
        rows = c.post(f'{API}/queue/submit', json={'claims': self.claims(3)}).json()['results']
        self.assertEqual([r['accepted'] for r in rows], [True, True, True])
        for r in rows:
            self.assertIn(r['lane'], ('green', 'A', 'B'))
            doc = self.qstore.get(r['claim_id'])
            self.assertEqual(doc['state'], 'triaged')
            self.assertEqual(doc['receipt']['lane'], r['lane'])

    def test_resubmitting_the_same_claim_does_not_make_a_second_version(self):
        c = self.w.login_client(4)
        body = {'claims': self.claims(1)}
        c.post(f'{API}/queue/submit', json=body)
        c.post(f'{API}/queue/submit', json=body)
        self.assertEqual(self.qstore.get(body['claims'][0]['claim_id'])['version'], 1)

    def test_one_bad_claim_is_reported_and_the_others_still_go_in(self):
        c = self.w.login_client(4)
        good = self.claims(2)
        bad = {'claim_id': 'has spaces!', 'nonsense': True}
        rows = c.post(f'{API}/queue/submit', json={'claims': [good[0], bad, good[1]]}).json()['results']
        self.assertEqual([r['accepted'] for r in rows], [True, False, True])
        self.assertTrue(rows[1]['reason'])
        self.assertIsNone(self.qstore.get('has spaces!'))

    def test_bodies_are_strict_and_bounded(self):
        c = self.w.login_client(4)
        self.assertEqual(c.post(f'{API}/queue/submit', json={'claims': []}).status_code, 422)
        self.assertEqual(c.post(f'{API}/queue/submit', json={'claims': self.claims(1), 'extra': 1}).status_code, 422)
        self.assertEqual(c.post(f'{API}/queue/submit', json={'claims': ['not an object']}).status_code, 422)
        self.assertEqual(c.post(f'{API}/queue/submit', json={'claims': [{'claim_id': 'x'}] * 26}).status_code, 422)

    def test_the_call_is_on_the_security_log_and_decides_nothing(self):
        c = self.w.login_client(4)
        c.post(f'{API}/queue/submit', json={'claims': self.claims(2)})
        self.assertIn(('queue_admin', 'submit_http'), [(e['event_type'], e.get('command')) for e in self.w.events()])
        self.assertEqual(self.qstore.counts().get('decided', 0), 0)

    def test_a_server_without_intake_refuses_cleanly(self):
        self.w.queue.intake = None
        c = self.w.login_client(4)
        self.assertEqual(c.post(f'{API}/queue/submit', json={'claims': self.claims(1)}).status_code, 422)


for _name, _factory in store_makers():
    globals()[f'Submit_{_name}'] = type(f'Submit_{_name}', (SubmitBase, unittest.TestCase), {'factory': staticmethod(_factory)})
del _name, _factory

if __name__ == '__main__':
    unittest.main()
