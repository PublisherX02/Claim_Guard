"""Data minimization: which persisted records may hold a patient or member identifier, checked against records a real flow wrote.

The claim body and the results' evidence in the queue store hold identifiers (the reviewer needs them, and the API masks them on the
way out). Everything else the queue writes must not: the security log, the review log, the deal documents and the dead letters.
docs/32 lists each record and who can read it; this test is what keeps that list true.
"""
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import masking
from wq_api_world import QueueWorld
from wq_world import store_makers

API = '/api/v1'
SNAPSHOT_KEYS = {'claim_id', 'version', 'state_at', 'score', 'eligibility', 'excluded_for', 'prior_deciders', 'avoid'}


def raw_in(obj, raw_ids):
    text = json.dumps(obj, ensure_ascii=False, default=str).lower()
    return sorted(r for r in raw_ids if r.lower() in text)


class MinimizationBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.w.on_shift('CG-2002', 'CG-2003', 'CG-3003', 'CG-3004', slice_size=4, low_water=1)

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def flow(self):
        """Ready a claim, deal it, let a reviewer decide it; returns (claim_id, the claim's raw identifiers)."""
        cid = self.w.ready(self.w.find('medium'))
        raw = list(masking.identifier_map(self.qstore.get(cid)['claim'], self.w.settings.pii_key))
        self.assertTrue(raw)
        self.w.queue.dispatcher.deal(full=True)
        holder = self.qstore.get(cid)['lease']['badge_id']
        rule = next(r['rule_id'] for r in self.qstore.get(cid)['results'] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'))
        client = self.w.login_client(badge=holder)
        r = client.post(f'{API}/work/claims/{cid}/findings/{rule}/decision',
                        json={'action': 'confirm_issue', 'reason': 'checked against the rule text'})
        self.assertEqual(r.status_code, 200, r.text)
        return cid, raw

    def test_no_identifier_in_the_logs_the_deals_or_the_dead_letters(self):
        cid, raw = self.flow()
        self.assertTrue(self.qstore.deals(100))
        for name, obj in (('security log', self.w.events()), ('review log', self.w.review_rows()),
                          ('deal documents', self.qstore.deals(100)), ('dead letters', self.qstore.dead_letters(100))):
            self.assertEqual(raw_in(obj, raw), [], name)

    def test_a_deal_keeps_only_what_the_plan_needs_from_a_claim(self):
        self.flow()
        for deal in self.qstore.deals(100):
            for snap in deal['claims']:
                self.assertEqual(set(snap), SNAPSHOT_KEYS)

    def test_the_conflict_of_interest_rule_survives_dropping_the_identifier(self):
        cid = self.w.ready(self.w.find('medium'))
        patient = self.qstore.get(cid)['claim']['patient_id']
        self.w.on_shift('CG-2002', 'CG-2003', slice_size=4, low_water=1, exclusions=(('CG-2002', patient),))
        self.w.queue.dispatcher.deal(full=True)
        deal = self.qstore.deals(1)[0]
        self.assertEqual(deal['claims'][0]['excluded_for'], ['CG-2002'])
        self.assertEqual([a[1] for a in deal['assigned']], ['CG-2003'])      # the barred reviewer is never dealt the claim
        self.assertEqual(raw_in(deal, [patient]), [])
        self.assertTrue(self.w.queue.dispatcher.verify(deal['deal_id']))     # and the plan is still reproducible from the stored inputs


for _name, _factory in store_makers():
    globals()[f'Minimization_{_name}'] = type(f'Minimization_{_name}', (MinimizationBase, unittest.TestCase),
                                              {'factory': staticmethod(_factory)})

if __name__ == '__main__':
    unittest.main()
