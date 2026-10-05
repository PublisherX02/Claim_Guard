"""Shadow mode: a stored prediction that acts on nothing, and an honest agreement figure with an exact interval."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from hypothesis import given, settings, strategies as st
from workqueue import shadow
from workqueue.intake import Intake
from wq_world import FakeEngine, World, good_claim, put_ready, store_makers


class IntervalTests(unittest.TestCase):
    def test_known_exact_intervals(self):
        lo, hi = shadow.clopper_pearson(5, 10)
        self.assertAlmostEqual(lo, 0.18709, places=4)
        self.assertAlmostEqual(hi, 0.81291, places=4)
        lo, hi = shadow.clopper_pearson(50, 50)
        self.assertAlmostEqual(lo, 0.025 ** (1 / 50), places=6)           # the closed form when every case agrees
        self.assertEqual(hi, 1.0)
        lo, hi = shadow.clopper_pearson(0, 50)
        self.assertEqual(lo, 0.0)
        self.assertAlmostEqual(hi, 1 - 0.025 ** (1 / 50), places=6)

    def test_no_cases_gives_no_bounds(self):
        self.assertEqual(shadow.clopper_pearson(0, 0), (None, None))

    def test_one_case(self):
        lo, hi = shadow.clopper_pearson(1, 1)
        self.assertAlmostEqual(lo, 0.025, places=6)
        self.assertEqual(hi, 1.0)
        lo, hi = shadow.clopper_pearson(0, 1)
        self.assertEqual(lo, 0.0)
        self.assertAlmostEqual(hi, 0.975, places=6)

    def test_bounds_never_decrease_as_the_number_of_agreements_grows(self):
        prev = (-1.0, -1.0)
        for k in range(0, 41):
            lo, hi = shadow.clopper_pearson(k, 40)
            self.assertGreaterEqual(lo, prev[0])
            self.assertGreaterEqual(hi, prev[1])
            prev = (lo, hi)

    def test_a_wider_confidence_gives_a_wider_interval_and_k_outside_n_is_refused(self):
        a, b = shadow.clopper_pearson(30, 50, 0.90), shadow.clopper_pearson(30, 50, 0.99)
        self.assertLess(b[0], a[0])
        self.assertGreater(b[1], a[1])
        for k, n in ((-1, 5), (6, 5)):
            with self.assertRaises(ValueError):
                shadow.clopper_pearson(k, n)

    @settings(max_examples=80, deadline=None)
    @given(st.integers(1, 400).flatmap(lambda n: st.tuples(st.integers(0, n), st.just(n))))
    def test_the_interval_always_contains_the_observed_rate_and_stays_inside_zero_and_one(self, kn):
        k, n = kn
        lo, hi = shadow.clopper_pearson(k, n)
        self.assertTrue(0.0 <= lo <= k / n <= hi <= 1.0)


class PredictorTests(unittest.TestCase):
    def test_only_a_sound_green_receipt_is_predicted_clear(self):
        self.assertEqual(shadow.predict_clear({'lane': 'green'}), 1.0)
        self.assertEqual(shadow.predict_clear({'lane': 'green', 'degraded': False}), 1.0)
        for receipt in ({'lane': 'A'}, {'lane': 'B'}, {'lane': 'green', 'degraded': True}, {}):
            self.assertEqual(shadow.predict_clear(receipt), 0.0)

    def test_a_prediction_must_be_a_probability(self):
        for bad in (1, 0, True, 1.5, -0.1, '1.0', None, float('nan')):
            with self.assertRaises(ValueError, msg=repr(bad)):
                shadow.record(None, 'C1', 1, bad, 1.0)


def decide(store, claim_id, actions, version=1, badge='B1'):
    """Walk a stored claim to decided with these finding actions ({rule: action}); no actions is a verified green claim."""
    store.lease(claim_id, version, badge, 5.0, 500.0)
    for rid, action in actions.items():
        store.add_decision(claim_id, version, badge, 6.0, {'rule_id': rid, 'action': action, 'actor': badge, 'reason': 'r', 'at': 6.0})
    store.transition(claim_id, version, 'leased', 'decided', badge, 7.0, set_fields={'decided_by': badge})


class AgreementBase:
    def setUp(self):
        self.store, self.cleanup = self.factory()

    def tearDown(self):
        self.cleanup()

    def stored(self, claim_id, lane, predicted, actions):
        put_ready(self.store, claim_id, lane=lane, eligibility='decide', score=0 if lane == 'green' else 4)
        self.assertTrue(shadow.record(self.store, claim_id, 1, predicted, 1.0))
        decide(self.store, claim_id, actions)

    def test_recording_changes_nothing_but_the_shadow_field_and_cannot_be_overwritten(self):
        put_ready(self.store, 'C1')
        before = self.store.get('C1')
        self.assertTrue(shadow.record(self.store, 'C1', 1, 0.0, 9.0))
        self.assertFalse(shadow.record(self.store, 'C1', 1, 1.0, 10.0))
        after = self.store.get('C1')
        self.assertEqual(after['shadow'], {'predicted_clear': 0.0, 'model': shadow.MODEL, 'at': 9.0})
        after['shadow'] = None
        self.assertEqual(after, before)

    def test_no_decided_claims_gives_no_bounds(self):
        self.assertEqual(shadow.agreement(self.store), {'n': 0, 'agree': 0, 'rate': None, 'confidence': 0.95, 'lower': None, 'upper': None,
                                                       'model': shadow.MODEL})

    def test_one_decided_claim(self):
        self.stored('C1', 'green', 1.0, {})
        out = shadow.agreement(self.store)
        self.assertEqual((out['n'], out['agree'], out['rate']), (1, 1, 1.0))
        self.assertAlmostEqual(out['lower'], 0.025, places=6)

    def test_fifty_agreeing_claims_have_the_stated_bounds(self):
        for i in range(50):
            self.stored(f'G{i:02d}', 'green', 1.0, {})
        out = shadow.agreement(self.store)
        self.assertEqual((out['n'], out['agree'], out['upper']), (50, 50, 1.0))
        self.assertTrue(0.92 < out['lower'] < 0.95)

    def test_a_hundred_claims_with_ten_disagreements(self):
        for i in range(60):
            self.stored(f'G{i:02d}', 'green', 1.0, {})                          # green, people verified clear: agree
        for i in range(30):
            self.stored(f'F{i:02d}', 'A', 0.0, {'R001': 'confirm_issue'})        # flagged, people confirmed: agree
        for i in range(10):
            self.stored(f'D{i:02d}', 'A', 0.0, {'R001': 'dismiss_with_reason'})   # flagged, people dismissed everything: disagree
        out = shadow.agreement(self.store)
        self.assertEqual((out['n'], out['agree'], out['rate']), (100, 90, 0.9))
        self.assertTrue(out['lower'] < 0.9 < out['upper'])

    def test_a_green_claim_the_people_found_something_wrong_with_is_a_disagreement(self):
        self.stored('G1', 'green', 1.0, {'R003': 'confirm_issue'})
        self.assertEqual(shadow.agreement(self.store)['agree'], 0)

    def test_a_later_decision_on_a_finding_replaces_the_earlier_one(self):
        self.stored('F1', 'A', 0.0, {})
        d = self.store.get('F1')
        self.assertTrue(shadow.human_cleared(d))
        d['decisions'] = [{'rule_id': 'R001', 'action': 'confirm_issue'}, {'rule_id': 'R001', 'action': 'dismiss_with_reason'}]
        self.assertTrue(shadow.human_cleared(d))
        d['decisions'].reverse()
        self.assertFalse(shadow.human_cleared(d))

    def test_claims_without_a_prediction_or_not_yet_decided_are_left_out(self):
        put_ready(self.store, 'NOPRED')
        decide(self.store, 'NOPRED', {})
        put_ready(self.store, 'OPEN')
        shadow.record(self.store, 'OPEN', 1, 0.0, 1.0)
        self.assertEqual(shadow.agreement(self.store)['n'], 0)

    def test_intake_records_one_prediction_per_new_claim_and_never_for_a_duplicate(self):
        world = World()
        try:
            intake = Intake(self.store, FakeEngine({'F': {'R001': ('FAIL', 'high')}}), world.log, world.clock, 'p', 'e')
            intake.submit(good_claim('G'))
            intake.submit(good_claim('G'))
            intake.submit(good_claim('F'))
            self.assertEqual((self.store.get('G')['shadow']['predicted_clear'], self.store.get('F')['shadow']['predicted_clear']), (1.0, 0.0))
        finally:
            world.close()

    def test_a_failing_shadow_write_never_stops_intake(self):
        world = World()
        try:
            def broken(*a, **k):
                raise RuntimeError('shadow store down')
            self.store.set_shadow = broken
            receipt = Intake(self.store, FakeEngine(), world.log, world.clock, 'p', 'e').submit(good_claim('G'))
            self.assertEqual(receipt['lane'], 'green')
            self.assertIsNone(self.store.get('G')['shadow'])
        finally:
            world.close()


def _make(name, factory):
    return type('Agreement_' + name, (AgreementBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Agreement_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
