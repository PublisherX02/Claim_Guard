"""The pipeline: every claim ends `ready` whatever the model does, and running it twice or late changes nothing."""
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import pipeline, states
from wq_world import World, build, good_claim, store_makers

FLAGS = {'G': {}, 'A': {'R001': ('FAIL', 'high')}, 'B': {'R001': ('FAIL', 'high'), 'R002': ('FAIL', 'high'), 'R003': ('FAIL', 'medium'),
                                                           'R004': ('FAIL', 'medium')}}


class PipelineBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.q = build(self.store, self.world, dict(FLAGS))

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def advance(self, claim_id):
        return pipeline.advance(self.store, claim_id, 1, self.q.rt.steps, self.world.clock())

    def test_a_green_claim_reaches_ready_with_no_model_call(self):
        self.q.intake.submit(good_claim('G'))
        self.assertEqual(self.advance('G'), 'ready')
        d = self.store.get('G')
        self.assertEqual((d['state'], self.q.model.calls), ('ready', 0))
        self.assertEqual([e['to'] for e in d['events']], ['triaged', 'ready'])
        self.assertEqual(d['events'][-1]['detail'], {'why': 'green'})

    def test_a_flagged_claim_passes_through_explained_to_ready(self):
        self.q.intake.submit(good_claim('A'))
        self.assertEqual(self.advance('A'), 'ready')
        d = self.store.get('A')
        self.assertEqual([e['to'] for e in d['events']], ['triaged', 'explained', 'ready'])
        self.assertEqual(self.q.model.calls, 1)

    def test_a_skipped_explanation_still_reaches_ready(self):
        self.q.model.script = ['fatal']
        self.q.intake.submit(good_claim('A'))
        self.assertEqual(self.advance('A'), 'ready')
        self.assertEqual([e['to'] for e in self.store.get('A')['events']], ['triaged', 'explanation_skipped', 'ready'])

    def test_running_it_twice_is_a_noop_the_second_time(self):
        self.q.intake.submit(good_claim('A'))
        self.assertEqual(self.advance('A'), 'ready')
        self.assertEqual(self.advance('A'), 'noop')
        self.assertEqual(len(self.store.get('A')['events']), 3)
        self.assertEqual(self.q.model.calls, 1)

    def test_a_claim_past_the_pipeline_is_left_alone(self):
        self.q.intake.submit(good_claim('G'))
        self.advance('G')
        self.store.lease('G', 1, 'B1', self.world.clock(), self.world.clock() + 100)
        self.assertEqual(self.advance('G'), 'noop')
        self.assertEqual(self.store.get('G')['state'], 'leased')
        self.assertEqual(self.advance('NOPE'), 'noop')

    def test_a_redelivery_after_a_crash_between_explanation_and_ready_does_not_call_the_model_again(self):
        self.q.intake.submit(good_claim('A'))
        self.q.explain.run('A', 1)                                   # the worker did this, then died before moving to ready
        self.assertEqual(self.store.get('A')['state'], 'explained')
        self.assertEqual(self.advance('A'), 'ready')
        self.assertEqual(self.q.model.calls, 1)

    def test_every_one_of_a_hundred_generated_claims_reaches_ready_whatever_the_model_does(self):
        rnd = random.Random(2026)
        behaviours = ['ok', 'ok', 'transient', 'timeout', 'fatal', 'slow', 'bug', '', None, 5]
        flags_pool = [FLAGS['G'], FLAGS['A'], FLAGS['B'], {'R005': ('UNABLE_TO_ASSESS', 'medium')}]
        for i in range(100):
            cid = f'P{i:03d}'
            self.q.flags_for[cid] = rnd.choice(flags_pool)
            self.q.intake.submit(good_claim(cid))
            self.q.model.script = [rnd.choice(behaviours) for _ in range(rnd.randint(0, 4))]
            self.q.breaker._state = 'closed'; self.q.breaker._streak = 0; self.q.breaker._calls.clear()
            self.assertEqual(self.advance(cid), 'ready', cid)
        counts = self.store.counts()
        self.assertEqual(sum(v for k, v in counts.items() if k.startswith('ready|')), 100)
        for d in self.store.by_state('ready', 200):
            chain = [e['to'] for e in d['events']]
            self.assertIn(chain, (['triaged', 'ready'], ['triaged', 'explained', 'ready'], ['triaged', 'explanation_skipped', 'ready']))


def _make(name, factory):
    return type('Pipeline_' + name, (PipelineBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Pipeline_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
