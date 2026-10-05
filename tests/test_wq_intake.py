"""Intake (one atomic write per claim version) and the outbox relay, on the in-memory store and, when configured, MongoDB."""
import copy
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import routing_config as rc, relay
from workqueue.intake import Intake
from wq_world import FakeEngine, World, good_claim, store_makers


class IntakeBase:
    flags = {'C2': {'R001': ('FAIL', 'high')}}

    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.engine = FakeEngine(self.flags)
        self.intake = self.new_intake()

    def new_intake(self, store=None):
        return Intake(store or self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'engine-1')

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def test_a_clean_claim_is_green_triaged_and_pending_in_the_outbox(self):
        receipt = self.intake.submit(good_claim('C1'))
        self.assertEqual((receipt['lane'], receipt['score'], receipt['eligibility']), ('green', 0, 'decide'))
        d = self.store.get('C1')
        self.assertEqual((d['state'], d['enqueue_pending'], d['version']), ('triaged', True, 1))
        self.assertEqual(d['receipt'], receipt)
        self.assertEqual((receipt['rule_pack_hash'], receipt['engine_version']), ('pack-1', 'engine-1'))
        events = self.world.events('triage_receipt')
        self.assertEqual(len(events), 1)
        self.assertEqual({k: events[0][k] for k in ('claim_id', 'lane', 'score', 'config_version')}, {'claim_id': 'C1', 'lane': 'green', 'score': 0, 'config_version': 1})
        self.assertEqual(events[0]['result_hash'], receipt['result_hash'])

    def test_a_flagged_claim_gets_its_lane_and_eligibility(self):
        receipt = self.intake.submit(good_claim('C2'))
        self.assertEqual((receipt['lane'], receipt['score'], receipt['eligibility']), ('A', 4, 'decide_high'))

    def test_a_duplicate_submit_returns_the_stored_receipt_and_does_nothing_else(self):
        first = self.intake.submit(good_claim('C1'))
        self.world.clock.advance(50)
        again = self.intake.submit(good_claim('C1'))
        self.assertEqual(again, first)
        self.assertEqual(self.engine.calls, 1)
        self.assertEqual(len(self.world.events('triage_receipt')), 1)
        self.assertEqual(self.store.counts(), {'triaged|green|decide': 1})

    def test_a_changed_body_is_version_two_with_a_new_receipt(self):
        r1 = self.intake.submit(good_claim('C1'))
        r2 = self.intake.submit(good_claim('C1', notes='A corrected resubmission.'))
        self.assertNotEqual(r1['input_hash'], r2['input_hash'])
        self.assertEqual((self.store.get('C1')['version'], self.store.get('C1', 1)['receipt']), (2, r1))
        self.assertEqual(len(self.world.events('triage_receipt')), 2)

    def test_going_back_to_an_earlier_body_returns_that_versions_receipt(self):
        r1 = self.intake.submit(good_claim('C1'))
        self.intake.submit(good_claim('C1', notes='second'))
        again = self.intake.submit(good_claim('C1'))
        self.assertEqual(again, r1)
        self.assertEqual(self.store.get('C1')['version'], 2)

    def test_a_malformed_claim_is_refused_before_any_write(self):
        broken = good_claim('C1'); del broken['lines']
        extra = good_claim('C1'); extra['state'] = 'decided'
        bad_date = good_claim('C1', submission_date='2026-13-45')
        weird_id = good_claim('../etc/passwd')
        no_id = good_claim('C1'); no_id['claim_id'] = {'$ne': None}
        for claim in (broken, extra, bad_date, weird_id, no_id, 'x', None, []):
            with self.assertRaises(ValueError, msg=repr(claim)[:50]):
                self.intake.submit(claim)
        self.assertEqual(self.store.counts(), {})
        self.assertEqual((self.engine.calls, self.world.events()), (0, []))

    def test_a_claim_that_is_not_plain_json_is_refused(self):
        with self.assertRaises(ValueError):
            self.intake.submit(good_claim('C1', total_amount=float('nan')))
        self.assertEqual(self.store.counts(), {})

    def test_the_receipt_names_the_configuration_in_force_and_a_new_one_applies_to_the_next_claim(self):
        self.assertEqual(self.intake.submit(good_claim('C1'))['config_version'], 1)
        self.assertTrue(self.store.put_config(rc.to_doc(rc.DEFAULT), 0))
        self.assertTrue(self.store.put_config(rc.to_doc(rc.validate(rc.RoutingConfig(version=2, lane_b_score=3))), 1))
        r = self.intake.submit(good_claim('C2'))
        self.assertEqual((r['config_version'], r['lane']), (2, 'B'))      # 4 points now reaches the lowered lane-B score

    def test_an_unreadable_stored_configuration_refuses_intake(self):
        self.store.put_config({'version': 1, 'slice_size': -5}, 0)
        with self.assertRaises(ValueError):
            self.intake.submit(good_claim('C1'))
        self.assertEqual(self.store.counts(), {})

    def test_a_crashed_publisher_loses_nothing(self):
        self.intake.submit(good_claim('C1'))
        other = self.new_intake(self.store)                           # a "restarted" process on the same store
        other.submit(good_claim('C3'))
        seen = []
        report = relay.sweep(self.store, lambda *a: seen.append(a), self.world.clock(), 60)
        self.assertEqual((report['published'], report['failed']), (2, 0))
        self.assertEqual(sorted(a[0] for a in seen), ['C1', 'C3'])

    def test_sweep_leaves_the_marker_when_the_publisher_fails_and_republishes_later(self):
        self.intake.submit(good_claim('C1'))

        def down(*a):
            raise ConnectionError('broker down')
        report = relay.sweep(self.store, down, self.world.clock(), 60)
        self.assertEqual((report['published'], report['failed']), (0, 1))
        self.assertTrue(self.store.get('C1')['enqueue_pending'])
        seen = []
        report = relay.sweep(self.store, lambda *a: seen.append(a), self.world.clock(), 60)
        self.assertEqual((report['published'], seen), (1, [('C1', 1, self.store.get('C1')['input_hash'])]))
        self.assertFalse(self.store.get('C1')['enqueue_pending'])

    def test_sweeping_twice_publishes_once(self):
        self.intake.submit(good_claim('C1'))
        seen = []
        relay.sweep(self.store, lambda *a: seen.append(a), self.world.clock(), 60)
        relay.sweep(self.store, lambda *a: seen.append(a), self.world.clock(), 60)
        self.assertEqual(len(seen), 1)

    def test_stale_markers_are_counted(self):
        self.intake.submit(good_claim('C1'))
        self.world.clock.advance(61)
        self.intake.submit(good_claim('C3'))
        report = relay.sweep(self.store, lambda *a: None, self.world.clock(), 60)
        self.assertEqual((report['published'], report['stale']), (2, 1))

    def test_a_publisher_that_raises_a_base_exception_is_not_swallowed(self):
        self.intake.submit(good_claim('C1'))

        def stop(*a):
            raise KeyboardInterrupt
        with self.assertRaises(KeyboardInterrupt):
            relay.sweep(self.store, stop, self.world.clock(), 60)
        self.assertTrue(self.store.get('C1')['enqueue_pending'])


def _make(name, factory):
    return type('Intake_' + name, (IntakeBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Intake_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
