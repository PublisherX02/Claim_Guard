"""Reconciliation: one orphan per check is reported exactly, and only an expired lease is repaired."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import leases, reconcile, states
from workqueue.intake import Intake
from wq_world import FakeEngine, World, good_claim, store_makers


class ReconcileBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.intake = Intake(self.store, FakeEngine({'F1': {'R001': ('FAIL', 'high')}}), self.world.log, self.world.clock, 'p', 'e')

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def to_ready(self, claim_id):
        self.intake.submit(good_claim(claim_id))
        now = self.world.clock()
        self.store.clear_outbox(claim_id, 1)
        self.assertIsNotNone(self.store.transition(claim_id, 1, 'triaged', 'ready', 'system:t', now))

    def findings(self, now=None, **limits):
        return reconcile.reconcile(self.store, self.world.clock() if now is None else now, limits)

    def test_a_healthy_queue_reports_nothing(self):
        self.to_ready('C1')
        self.store.lease('C1', 1, 'B1', self.world.clock(), self.world.clock() + 1800)
        report = self.findings()
        self.assertEqual((report.ok, report.findings), (True, []))

    def test_an_empty_store_is_ok(self):
        self.assertTrue(self.findings().ok)

    def test_a_document_stuck_in_triaged_is_reported_not_repaired(self):
        self.intake.submit(good_claim('C1'))
        self.store.clear_outbox('C1', 1)
        report = self.findings(now=self.world.clock() + 61)
        self.assertEqual([(f['check'], f['claim_id'], f['repaired']) for f in report.findings], [('stuck', 'C1', False)])
        self.assertFalse(report.ok)
        self.assertEqual(self.store.get('C1')['state'], 'triaged')
        self.assertTrue(self.findings(now=self.world.clock() + 60).ok)           # not yet stuck at exactly the limit

    def test_a_document_stuck_in_explained_or_skipped_is_reported(self):
        self.intake.submit(good_claim('C1')); self.intake.submit(good_claim('C2'))
        for cid in ('C1', 'C2'):
            self.store.clear_outbox(cid, 1)
        self.store.transition('C1', 1, 'triaged', 'explained', 'system:t', self.world.clock())
        self.store.transition('C2', 1, 'triaged', 'explanation_skipped', 'system:t', self.world.clock())
        got = sorted((f['claim_id'], f['state']) for f in self.findings(now=self.world.clock() + 100).findings)
        self.assertEqual(got, [('C1', 'explained'), ('C2', 'explanation_skipped')])

    def test_a_claim_waiting_in_ready_for_a_day_is_reported_not_repaired(self):
        self.to_ready('C1')
        report = self.findings(now=self.world.clock() + 86401)
        self.assertEqual([(f['check'], f['repaired']) for f in report.findings], [('waiting_too_long', False)])
        self.assertEqual(self.store.get('C1')['state'], 'ready')

    def test_an_unpublished_marker_is_reported(self):
        self.intake.submit(good_claim('C1'))
        self.store.transition('C1', 1, 'triaged', 'ready', 'system:t', self.world.clock())
        report = self.findings(now=self.world.clock() + 61)
        self.assertEqual([f['check'] for f in report.findings], ['unpublished_marker'])
        self.assertEqual(self.findings(now=self.world.clock() + 10).findings, [])

    def test_an_expired_lease_is_repaired_through_the_normal_hand_back(self):
        self.to_ready('C1')
        t = self.world.clock()
        self.store.lease('C1', 1, 'B1', t, t + 100)
        report = self.findings(now=t + 100)
        self.assertEqual([(f['check'], f['repaired'], f['badge_id']) for f in report.findings], [('lease_expired', True, 'B1')])
        self.assertTrue(report.ok)
        d = self.store.get('C1')
        self.assertEqual((d['state'], d['lease']), ('ready', None))
        last = d['events'][-1]
        self.assertEqual((last['from'], last['to'], last['actor']), ('leased', 'ready', 'system:reconcile'))
        self.assertEqual(last['detail'], {'event': 'lease_expired', 'badge_id': 'B1'})

    def test_an_unexpired_lease_is_left_alone(self):
        self.to_ready('C1')
        t = self.world.clock()
        self.store.lease('C1', 1, 'B1', t, t + 100)
        self.assertEqual(self.findings(now=t + 99).findings, [])
        self.assertEqual(self.store.get('C1')['state'], 'leased')

    def test_a_decision_that_wins_the_race_is_not_undone_by_the_repair(self):
        self.to_ready('C1')
        t = self.world.clock()
        doc = self.store.lease('C1', 1, 'B1', t, t + 100)
        self.store.transition('C1', 1, 'leased', 'decided', 'B1', t + 50, set_fields={'decided_by': 'B1'})
        self.assertIsNone(leases.release(self.store, doc, t + 100, 'system:reconcile'))      # stale view of the document
        self.assertEqual(self.store.get('C1')['state'], 'decided')

    def test_two_versions_of_one_claim_leased_at_once_are_reported(self):
        for version in (1, 2):
            self.intake.submit(good_claim('C1', notes=f'v{version}'))
            self.store.clear_outbox('C1', version)
            self.store.transition('C1', version, 'triaged', 'ready', 'system:t', self.world.clock())
            self.store.lease('C1', version, f'B{version}', self.world.clock(), self.world.clock() + 1800)
        report = self.findings()
        self.assertEqual([(f['check'], f['versions']) for f in report.findings], [('two_live_leases', [1, 2])])

    def test_a_decided_claim_without_a_person_is_reported(self):
        self.to_ready('C1')
        t = self.world.clock()
        self.store.lease('C1', 1, 'B1', t, t + 100)
        self.store.transition('C1', 1, 'leased', 'decided', 'B1', t + 1)                  # no decided_by recorded
        self.assertEqual([f['check'] for f in self.findings().findings], ['decided_without_a_person'])

    def test_one_orphan_per_check_gives_exactly_those_findings(self):
        self.intake.submit(good_claim('S1')); self.store.clear_outbox('S1', 1)                # stuck in triaged
        self.intake.submit(good_claim('M1'))                                                    # unpublished marker, still fresh? no
        self.to_ready('E1'); t = self.world.clock(); self.store.lease('E1', 1, 'B1', t, t + 100)  # expired lease
        report = self.findings(now=t + 200)
        got = sorted((f['check'], f['claim_id']) for f in report.findings)
        self.assertEqual(got, [('lease_expired', 'E1'), ('stuck', 'M1'), ('stuck', 'S1'), ('unpublished_marker', 'M1')])
        self.assertEqual([f['claim_id'] for f in report.findings if f['repaired']], ['E1'])
        self.assertEqual(self.store.get('S1')['state'], 'triaged')
        self.assertEqual(self.store.get('E1')['state'], 'ready')

    def test_repairing_twice_changes_nothing_the_second_time(self):
        self.to_ready('C1')
        t = self.world.clock()
        self.store.lease('C1', 1, 'B1', t, t + 100)
        self.assertEqual(len(self.findings(now=t + 200).findings), 1)
        self.assertEqual(self.findings(now=t + 200).findings, [])
        self.assertEqual(len(self.store.get('C1')['events']), 4)       # received->triaged, triaged->ready, ready->leased, leased->ready: no second repair event


class UnknownStateTests(unittest.TestCase):
    def test_a_document_in_a_state_the_machine_does_not_know_is_reported(self):
        from workqueue.store import MemoryQueueStore
        world = World()
        try:
            store = MemoryQueueStore()
            Intake(store, FakeEngine(), world.log, world.clock, 'p', 'e').submit(good_claim('C1'))
            store._docs[('C1', 1)]['state'] = 'mystery'
            report = reconcile.reconcile(store, world.clock())
            checks = sorted(f['check'] for f in report.findings)
            self.assertEqual(checks, ['count_mismatch', 'unknown_state'])
            self.assertFalse(report.ok)
        finally:
            world.close()


def _make(name, factory):
    return type('Reconcile_' + name, (ReconcileBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Reconcile_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
