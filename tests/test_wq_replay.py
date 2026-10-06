"""Replay, rerun, dead-letter replay, advisory results and the database history for the extension rules."""
import copy
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import pipeline, replay, tasks, triage
from workqueue.history import StoreHistory
from workqueue.intake import Intake
from workqueue.service import Conflict, NotFound
from wq_world import FakeEngine, World, build, good_claim, results_of, store_makers

CHANGED = {'R001': ('FAIL', 'high')}


class Engine:
    """A fake engine whose verdicts can change between 'rule packs'."""

    def __init__(self, flags_for=None):
        self.flags_for = dict(flags_for or {})

    def __call__(self, claim):
        return results_of(self.flags_for.get(claim['claim_id']), claim['claim_id'])


class ReplayBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.engine = Engine({'A': CHANGED})
        self.intake = Intake(self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'e')

    def tearDown(self):
        self.cleanup()
        self.world.close()

    # ---- replay
    def test_an_untouched_claim_replays_the_same(self):
        self.intake.submit(good_claim('A'))
        out = replay.replay_claim(self.store, self.engine, 'A')
        self.assertEqual((out['same'], out['diff'], out['claim_intact'], out['results_intact']), (True, [], True, True))
        self.assertEqual(out['old_hash'], out['new_hash'])

    def test_a_changed_rule_is_reported_in_the_diff(self):
        self.intake.submit(good_claim('A'))
        self.engine.flags_for['A'] = {'R001': ('PASS', 'high'), 'R002': ('FAIL', 'medium')}
        out = replay.replay_claim(self.store, self.engine, 'A')
        self.assertFalse(out['same'])
        self.assertEqual([(d['rule_id'], d['was'][0], d['now'][0]) for d in out['diff']], [('R001', 'FAIL', 'PASS'), ('R002', 'PASS', 'FAIL')])

    def test_replay_notices_a_stored_claim_or_result_that_was_edited_afterwards(self):
        self.intake.submit(good_claim('A'))
        self.damage_claim('A')
        out = replay.replay_claim(self.store, self.engine, 'A')
        self.assertEqual((out['claim_intact'], out['results_intact']), (False, True))
        self.damage_results('A')
        out = replay.replay_claim(self.store, self.engine, 'A')
        self.assertEqual((out['claim_intact'], out['results_intact']), (False, False))

    def test_replaying_an_unknown_claim_or_version_is_not_found(self):
        with self.assertRaises(NotFound):
            replay.replay_claim(self.store, self.engine, 'NOPE')
        self.intake.submit(good_claim('A'))
        with self.assertRaises(NotFound):
            replay.replay_claim(self.store, self.engine, 'A', 7)

    # ---- rerun
    def new_pack(self, flags):
        engine = Engine(flags)
        return engine, Intake(self.store, engine, self.world.log, self.world.clock, 'pack-2', 'e')

    def test_a_dry_run_lists_the_claims_that_would_change_and_changes_nothing(self):
        for cid in ('A', 'B', 'C'):
            self.intake.submit(good_claim(cid))
        engine2, intake2 = self.new_pack({'A': CHANGED, 'B': {'R002': ('FAIL', 'medium')}, 'C': {}})      # A unchanged, B changed, C unchanged
        out = replay.rerun(self.store, engine2, 'pack-1', intake2)
        self.assertEqual((out['dry_run'], out['checked'], out['changed'], out['unchanged'], out['created']), (True, 3, ['B'], 2, []))
        self.assertEqual(self.store.get('B')['version'], 1)
        self.assertEqual(self.store.counts(), {'triaged|green|decide': 2, 'triaged|A|decide_high': 1})

    def test_applying_creates_version_two_only_for_changed_claims_and_keeps_the_old_decisions(self):
        for cid in ('A', 'B'):
            self.intake.submit(good_claim(cid))
        self.store.transition('A', 1, 'triaged', 'ready', 'system:t', 1.0)
        self.store.lease('A', 1, 'B1', 2.0, 100.0)
        self.store.add_decision('A', 1, 'B1', 3.0, {'rule_id': 'R001', 'action': 'confirm_issue', 'actor': 'B1', 'reason': 'x', 'at': 3.0})
        self.store.transition('A', 1, 'leased', 'decided', 'B1', 4.0, set_fields={'decided_by': 'B1'})
        engine2, intake2 = self.new_pack({'A': {'R002': ('FAIL', 'medium')}, 'B': {}})
        out = replay.rerun(self.store, engine2, 'pack-1', intake2, dry_run=False)
        self.assertEqual((out['changed'], out['created']), (['A'], [{'claim_id': 'A', 'version': 2}]))
        v1, v2 = self.store.get('A', 1), self.store.get('A', 2)
        self.assertEqual((v1['state'], v1['decided_by'], len(v1['decisions'])), ('decided', 'B1', 1))          # history untouched
        self.assertEqual((v2['state'], v2['receipt']['rule_pack_hash'], v2['decisions']), ('triaged', 'pack-2', []))
        self.assertEqual(self.store.get('B')['version'], 1)
        self.assertEqual(self.store.get('B')['receipt']['rule_pack_hash'], 'pack-1')

    def test_a_second_apply_finds_nothing_left_to_change(self):
        self.intake.submit(good_claim('A'))
        engine2, intake2 = self.new_pack({'A': {}})
        replay.rerun(self.store, engine2, 'pack-1', intake2, dry_run=False)
        again = replay.rerun(self.store, engine2, 'pack-1', intake2, dry_run=False)
        self.assertEqual((again['checked'], again['created']), (0, []))

    def test_rerunning_with_the_pack_that_is_already_running_is_refused(self):
        with self.assertRaises(ValueError):
            replay.rerun(self.store, self.engine, 'pack-1', self.intake)

    def test_a_claim_the_new_engine_cannot_evaluate_is_reported_not_fatal(self):
        for cid in ('A', 'B'):
            self.intake.submit(good_claim(cid))

        def broken(claim):
            if claim['claim_id'] == 'A':
                raise RuntimeError('engine bug')
            return results_of({'R002': ('FAIL', 'medium')}, claim['claim_id'])
        intake2 = Intake(self.store, broken, self.world.log, self.world.clock, 'pack-2', 'e')
        out = replay.rerun(self.store, broken, 'pack-1', intake2, dry_run=False)
        self.assertEqual((out['errors'], out['changed']), (['A'], ['B']))
        self.assertEqual(self.store.get('A')['version'], 1)

    def test_a_superseded_version_is_never_dealt_and_its_lease_is_reclaimed(self):
        from workqueue.dispatcher import Agent, Dispatcher
        from wq_world import set_cfg
        self.intake.submit(good_claim('A'))
        self.store.transition('A', 1, 'triaged', 'ready', 'system:t', 1.0)
        perms = frozenset({'claims.decide', 'claims.decide_high'})
        set_cfg(self.store, on_shift=('B1', 'B2'))
        d = Dispatcher(self.store, lambda: [Agent('B1', perms), Agent('B2', perms)], self.world.clock, rng_seed=1)
        self.assertEqual(len(d.deal(now=10.0).assigned), 1)                   # version 1 is leased
        engine2, intake2 = self.new_pack({'A': {'R002': ('FAIL', 'medium')}})
        replay.rerun(self.store, engine2, 'pack-1', intake2, dry_run=False)
        self.assertEqual(d.expire(now=11.0), 1)                               # the lease on the old version is taken back
        self.assertEqual(self.store.get('A', 1)['state'], 'ready')
        self.store.transition('A', 2, 'triaged', 'ready', 'system:t', 12.0)
        deal = d.deal(now=13.0)
        self.assertEqual([(c, v) for c, _, v in deal.assigned], [('A', 2)])   # only the newest version is dealt

    # ---- dead letters
    def test_dead_letter_replay_moves_the_claim_back_once_and_rearms_the_publish(self):
        q = build(self.store, self.world, {'A': CHANGED}, guard=self.exploding)
        q.rt.max_retries = 1
        app = tasks.make_app('memory://', eager=True, runtime=q.rt)
        ih = q.intake.submit(good_claim('A'))['input_hash']
        self.assertEqual(app.tasks[tasks.PROCESS].delay('A', 1, ih).result, 'dead_lettered')
        dead = self.store.dead_letters()[0]['dead_id']
        self.store.clear_outbox('A', 1)
        moved = replay.replay_dead_letter(self.store, dead, self.world.clock(), 'CG-4004')
        self.assertEqual((moved['state'], moved['enqueue_pending']), ('triaged', True))
        self.assertEqual(self.store.dead_letters(), [])
        with self.assertRaises(NotFound):
            replay.replay_dead_letter(self.store, dead, self.world.clock(), 'CG-4004')
        self.assertEqual(self.store.get('A')['events'][-1]['detail'], {'event': 'dead_letter_replay', 'dead_id': dead})

    @staticmethod
    def exploding(text, result):
        raise RuntimeError('guard bug')

    def test_a_dead_letter_for_a_claim_that_moved_on_is_a_conflict_and_stays_listed(self):
        self.intake.submit(good_claim('A'))
        self.store.transition('A', 1, 'triaged', 'ready', 'system:t', 1.0)
        self.store.add_dead_letter({'dead_id': 'X1', 'claim_id': 'A', 'version': 1, 'reason': 'boom', 'at': 1.0})
        with self.assertRaises(Conflict):
            replay.replay_dead_letter(self.store, 'X1', 2.0, 'CG-4004')
        self.assertEqual(len(self.store.dead_letters()), 1)

    # ---- advisory results and database history
    def test_advisory_results_are_stored_but_never_change_the_routing(self):
        plain = Intake(self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'e')
        with_history = Intake(self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'e', history=StoreHistory(self.store))
        a = plain.submit(good_claim('A', patient='P9'))
        b = with_history.submit(good_claim('A2', patient='P9'))
        self.assertEqual(self.store.get('A')['advisory'], [])
        advice = self.store.get('A2')['advisory']
        self.assertEqual(len(advice), 8)
        self.assertEqual(sorted(r['rule_id'] for r in advice), sorted(['E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103']))
        # the official lane, score and eligibility depend only on the official results
        c = Intake(self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'e',
                   advisory=lambda claim, history: [{'rule_id': 'E999', 'status': 'FAIL', 'severity': 'high'}]).submit(good_claim('A3', patient='P9'))
        for r in (a, b, c):
            self.assertEqual((r['lane'], r['score'], r['eligibility']), ('A', 4, 'decide_high') if r['claim_id'] == 'A' else (r['lane'], r['score'], r['eligibility']))
        self.assertEqual((b['lane'], b['score'], b['eligibility']), (c['lane'], c['score'], c['eligibility']))
        self.assertEqual((a['lane'], a['score'], a['eligibility']), ('A', 4, 'decide_high'))

    def test_advisory_results_reach_the_reviewers_view_masked_and_are_not_decidable(self):
        from wq_api_world import QueueWorld
        w = QueueWorld(self.store)
        try:
            w.staff()
            w.intake = Intake(self.store, w.intake.engine, w.log, w.clock, 'pack', 'engine', history=StoreHistory(self.store))
            cid = w.ready(w.find('medium'))
            self.assertEqual(len(self.store.get(cid)['advisory']), 8)
            w.on_shift('CG-2002', slice_size=4, low_water=1)
            w.lease_to(cid, 'CG-2002')
            view = w.queue.inbox(w.principal(badge='CG-2002'))[0]
            self.assertEqual(sorted(r['rule_id'] for r in view['advisory']), sorted(['E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103']))
            self.assertTrue(all('allowed_actions' not in r for r in view['advisory']))
            raw = w.claims.get(cid)[0]['patient_id']
            self.assertFalse(raw in str(view))
        finally:
            w.close()

    def test_a_crashing_advisory_step_never_stops_intake(self):
        def boom(claim, history):
            raise RuntimeError('advisory bug')
        r = Intake(self.store, self.engine, self.world.log, self.world.clock, 'pack-1', 'e', advisory=boom).submit(good_claim('B'))
        self.assertEqual(r['lane'], 'green')
        self.assertEqual(self.store.get('B')['advisory'], [])

    def test_the_history_over_the_database_gives_the_same_extension_verdicts_as_the_batch_history(self):
        from claim_history import InMemoryHistory
        from engine_core import load_jsonl
        from extension_rules import evaluate_extensions
        claims = []
        for split in ('development', 'validation', 'stress'):
            claims += [dict(c, claim_id=f'{split[:3]}-{c["claim_id"]}') for c in load_jsonl(str(ROOT / 'data' / split / 'claims.jsonl'))]
        claims += [dict(c, claim_id='shr-' + c['claim_id'], patient_id=f'SHARED-{i % 40}') for i, c in enumerate(claims)]
        for i, c in enumerate(claims):
            doc = {'claim_id': c['claim_id'], 'version': 1, 'input_hash': f'h{i}', 'claim': c, 'results': [],
                   'receipt': {'lane': 'green', 'eligibility': 'decide', 'score': 0, 'created_at': 1.0}}
            self.assertTrue(self.store.put_triaged(doc))
        batch, database = InMemoryHistory(claims), StoreHistory(self.store)
        different, with_history = [], 0
        for c in claims:
            want = {r['rule_id']: r['status'] for r in evaluate_extensions(c, batch)}
            got = {r['rule_id']: r['status'] for r in evaluate_extensions(c, database)}
            if want != got:
                different.append(c['claim_id'])
            with_history += any(want[k] != 'PASS' for k in ('E101', 'E102', 'E103'))
        self.assertEqual(different, [])
        self.assertGreater(with_history, 0, 'the fixture should exercise the history rules')


def _make(name, factory):
    if name == 'memory':
        def damage_claim(self, cid):
            self.store._docs[(cid, 1)]['claim']['notes'] = 'edited in the database'

        def damage_results(self, cid):
            self.store._docs[(cid, 1)]['results'][1]['status'] = 'FAIL'
    else:
        def damage_claim(self, cid):
            self.store.raw_db['claims'].update_one({'claim_id': cid}, {'$set': {'claim.notes': 'edited in the database'}})

        def damage_results(self, cid):
            self.store.raw_db['claims'].update_one({'claim_id': cid}, {'$set': {'results.1.status': 'FAIL'}})
    return type('Replay_' + name, (ReplayBase, unittest.TestCase),
                {'factory': staticmethod(factory), 'damage_claim': damage_claim, 'damage_results': damage_results})


for _name, _factory in store_makers():
    globals()['Replay_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
