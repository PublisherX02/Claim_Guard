"""The Celery tasks run eagerly (no broker, no worker process): the adapter wiring, idempotency and the beat schedule."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import tasks
from workqueue.dispatcher import Agent
from wq_world import World, build, good_claim, set_cfg, store_makers

FLAGS = {'G': {}, 'A': {'R001': ('FAIL', 'high')}}
L3 = frozenset({'claims.decide', 'claims.decide_high'})


class EagerBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.q = build(self.store, self.world, dict(FLAGS))
        self.app = tasks.make_app('memory://', eager=True, runtime=self.q.rt)
        self.process = self.app.tasks[tasks.PROCESS]

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def test_the_app_is_configured_for_safe_delivery(self):
        c = self.app.conf
        self.assertTrue(c.task_acks_late)
        self.assertTrue(c.task_reject_on_worker_lost)
        self.assertEqual(c.broker_transport_options['visibility_timeout'], 3600)
        self.assertGreater(c.broker_transport_options['visibility_timeout'], 90 * 3 + 60)       # longer than the AI ceiling and its retries
        self.assertEqual((c.task_serializer, c.result_serializer, list(c.accept_content)), ('json', 'json', ['json']))
        self.assertTrue(c.task_always_eager)

    def test_it_is_not_eager_unless_asked(self):
        self.assertFalse(tasks.make_app('memory://').conf.task_always_eager)

    def test_the_beat_schedule_is_the_stated_one(self):
        sched = {k: (v['task'], v['schedule']) for k, v in self.app.conf.beat_schedule.items()}
        self.assertEqual(sched, {'deal': ('workqueue.deal', 15.0), 'expire': ('workqueue.expire', 60.0),
                                 'relay': ('workqueue.relay_sweep', 10.0), 'reconcile': ('workqueue.reconcile', 300.0)})
        for name, _ in sched.values():
            self.assertIn(name, self.app.tasks)

    def test_process_claim_takes_a_claim_to_ready(self):
        ih = self.q.intake.submit(good_claim('A'))['input_hash']
        res = self.process.delay('A', 1, ih)
        self.assertEqual(res.result, 'ready')
        self.assertEqual(self.store.get('A')['state'], 'ready')

    def test_a_message_with_the_wrong_input_hash_or_an_unknown_claim_is_a_noop(self):
        self.q.intake.submit(good_claim('A'))
        self.assertEqual(self.process.delay('A', 1, 'f' * 64).result, 'noop')
        self.assertEqual(self.process.delay('NOPE', 1, 'x').result, 'noop')
        self.assertEqual(self.store.get('A')['state'], 'triaged')
        self.assertEqual(self.q.model.calls, 0)

    def test_the_relay_publishes_pending_claims_through_the_real_publisher(self):
        from workqueue import relay
        self.q.intake.submit(good_claim('A')); self.q.intake.submit(good_claim('G'))
        report = relay.sweep(self.store, tasks.publisher(self.app), self.world.clock())
        self.assertEqual(report['published'], 2)
        self.assertEqual(self.store.counts(), {'ready|A|decide_high': 1, 'ready|green|decide': 1})
        self.assertEqual(self.store.pending_outbox(10), [])

    def test_the_deal_task_deals_and_the_expire_task_takes_leases_back(self):
        set_cfg(self.store, on_shift=('B1',), slice_size=3, low_water=1)
        self.q.dispatcher.agents = lambda: [Agent('B1', L3)]
        ih = self.q.intake.submit(good_claim('G'))['input_hash']
        self.process.delay('G', 1, ih)
        self.assertEqual(self.app.tasks['workqueue.deal'].delay().result, 1)
        self.assertEqual(len(self.store.inbox('B1')), 1)
        self.world.clock.advance(1801)
        self.assertEqual(self.app.tasks['workqueue.expire'].delay().result, 1)
        self.assertEqual(self.store.get('G')['state'], 'ready')

    def test_the_reconcile_task_reports(self):
        self.assertEqual(self.app.tasks['workqueue.reconcile'].delay().result, {'ok': True, 'findings': 0})
        self.q.intake.submit(good_claim('G'))
        self.world.clock.advance(120)
        self.assertEqual(self.app.tasks['workqueue.reconcile'].delay().result, {'ok': False, 'findings': 2})   # stuck, marker unpublished


def _make(name, factory):
    return type('Eager_' + name, (EagerBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Eager_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
