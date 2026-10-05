"""Fault injection: duplicate and late deliveries, a worker that dies half way, a broker that is gone, a model that misbehaves, and a
guard that crashes. No claim may be lost, repeated or left without an owner. The Redis-backed tests need REDIS_URL and are skipped
loudly without it (REQUIRE_REDIS=1 turns a missing URL into a failure)."""
import os
import sys
import threading
import time
import unittest
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import relay, tasks
from wq_world import World, build, good_claim, store_makers

REDIS_URL = os.environ.get('REDIS_URL')
SKIP_REDIS = 'REDIS_URL not set: the Redis-backed queue tests were NOT run'
FLAGS = {'G': {}, 'A': {'R001': ('FAIL', 'high')}, 'A2': {'R001': ('FAIL', 'high')}}


class RedisIsConfigured(unittest.TestCase):
    def test_the_broker_is_configured_when_it_is_required(self):
        if os.environ.get('REQUIRE_REDIS') == '1':
            self.assertTrue(REDIS_URL, 'REQUIRE_REDIS=1 but REDIS_URL is not set')
        elif not REDIS_URL:
            self.skipTest(SKIP_REDIS)


class FaultBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.q = build(self.store, self.world, dict(FLAGS))
        self.app = tasks.make_app('memory://', eager=True, runtime=self.q.rt)
        self.process = self.app.tasks[tasks.PROCESS]

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def submit(self, claim_id):
        return self.q.intake.submit(good_claim(claim_id))['input_hash']

    def count_events(self, claim_id, to):
        return sum(1 for e in self.store.get(claim_id)['events'] if e['to'] == to)

    # ---- delivery
    def test_the_same_message_delivered_three_times_explains_once(self):
        ih = self.submit('A')
        results = [self.process.delay('A', 1, ih).result for _ in range(3)]
        self.assertEqual(results, ['ready', 'noop', 'noop'])
        self.assertEqual((self.count_events('A', 'explained'), self.count_events('A', 'ready'), self.q.model.calls), (1, 1, 1))

    def test_a_worker_that_dies_after_the_write_is_redelivered_without_repeating_the_model_call(self):
        ih = self.submit('A')
        real, died = self.q.explain.run, []

        def dies_after_writing(claim_id, version):
            out = real(claim_id, version)
            if not died:
                died.append(1)
                raise ConnectionError('worker killed after the write')
            return out
        self.q.explain.run = dies_after_writing
        self.assertEqual(self.process.delay('A', 1, ih).result, 'ready')       # the retry is the redelivery
        self.assertEqual((self.q.model.calls, self.count_events('A', 'explained'), self.store.get('A')['state']), (1, 1, 'ready'))

    def test_a_message_for_a_claim_already_leased_is_a_noop(self):
        ih = self.submit('G')
        self.process.delay('G', 1, ih)
        self.store.lease('G', 1, 'B1', self.world.clock(), self.world.clock() + 100)
        self.assertEqual(self.process.delay('G', 1, ih).result, 'noop')
        self.assertEqual(self.store.get('G')['state'], 'leased')

    def test_a_message_for_a_decided_or_dead_lettered_claim_is_a_noop(self):
        ih = self.submit('G')
        self.process.delay('G', 1, ih)
        self.store.lease('G', 1, 'B1', 1.0, 100.0)
        self.store.transition('G', 1, 'leased', 'decided', 'B1', 2.0, set_fields={'decided_by': 'B1'})
        self.assertEqual(self.process.delay('G', 1, ih).result, 'noop')
        ih2 = self.submit('A')
        self.store.transition('A', 1, 'triaged', 'dead_lettered', 'system:t', 3.0)
        self.assertEqual(self.process.delay('A', 1, ih2).result, 'noop')

    # ---- the broker
    def test_when_the_broker_is_gone_the_markers_stay_and_the_relay_republishes_after_recovery(self):
        self.submit('A'); self.submit('G')
        down = tasks.make_app('redis://127.0.0.1:1/0', runtime=self.q.rt)          # nothing listens there
        started = time.monotonic()
        report = relay.sweep(self.store, tasks.publisher(down), self.world.clock())
        self.assertLess(time.monotonic() - started, 30, 'a dead broker must fail fast, not hang the relay')
        self.assertEqual((report['published'], report['failed']), (0, 2))
        self.assertEqual(len(self.store.pending_outbox(10)), 2)
        self.assertEqual(self.store.counts(), {'triaged|A|decide_high': 1, 'triaged|green|decide': 1})       # nothing was lost
        report = relay.sweep(self.store, tasks.publisher(self.app), self.world.clock())          # recovery
        self.assertEqual(report['published'], 2)
        self.assertEqual(self.store.counts(), {'ready|A|decide_high': 1, 'ready|green|decide': 1})

    def test_a_crash_between_the_write_and_the_publish_loses_nothing(self):
        self.submit('A')                                          # written; the process "dies" before publishing anything
        fresh = build(self.store, self.world, dict(FLAGS))        # a restarted process on the same database
        app = tasks.make_app('memory://', eager=True, runtime=fresh.rt)
        relay.sweep(self.store, tasks.publisher(app), self.world.clock())
        self.assertEqual(self.store.get('A')['state'], 'ready')

    # ---- the model
    def test_a_model_that_times_out_or_answers_too_late_still_ends_ready(self):
        for cid, script in (('A', ['timeout'] * 3), ('A2', ['slow'])):
            ih = self.submit(cid)
            self.q.model.script = list(script)
            self.q.breaker._state = 'closed'; self.q.breaker._streak = 0; self.q.breaker._calls.clear()
            self.assertEqual(self.process.delay(cid, 1, ih).result, 'ready', cid)
            self.assertEqual(self.store.get(cid)['state'], 'ready')
            self.assertEqual(self.store.get(cid)['explanation']['outcome'], 'skipped_timeout')

    def test_over_budget_and_a_refusing_guard_still_end_ready(self):
        import dataclasses
        from workqueue import routing_config as rc
        self.q.cfg = dataclasses.replace(rc.DEFAULT, ai_daily_budget=0)
        ih = self.submit('A')
        self.assertEqual(self.process.delay('A', 1, ih).result, 'ready')
        self.assertEqual(self.store.get('A')['explanation']['outcome'], 'skipped_budget')
        self.q.cfg = rc.DEFAULT
        self.q.explain.guard = lambda text, result: False
        ih2 = self.submit('A2')
        self.assertEqual(self.process.delay('A2', 1, ih2).result, 'ready')
        self.assertEqual(self.store.get('A2')['explanation']['outcome'], 'skipped_guard')

    def test_a_crash_inside_the_guard_dead_letters_the_claim_after_the_retries_with_one_dead_letter(self):
        def exploding(text, result):
            raise RuntimeError('the guard has a bug with this text')
        self.q.explain.guard = exploding
        ih = self.submit('A')
        self.assertEqual(self.process.delay('A', 1, ih).result, 'dead_lettered')
        self.assertEqual(self.q.model.calls, 1 + self.q.rt.max_retries)
        d = self.store.get('A')
        self.assertEqual((d['state'], d['events'][-1]['detail']), ('dead_lettered', {'reason': 'RuntimeError', 'attempts': 4}))
        letters = self.store.dead_letters()
        self.assertEqual(len(letters), 1)
        self.assertEqual((letters[0]['claim_id'], letters[0]['reason'], letters[0]['attempts']), ('A', 'RuntimeError', 4))
        self.assertNotIn('bug', repr(letters))                    # the error text is never stored, only its type
        self.assertEqual(self.process.delay('A', 1, ih).result, 'noop')


def _make(name, factory):
    return type('Faults_' + name, (FaultBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Faults_' + _name] = _make(_name, _factory)
del _name, _factory


@unittest.skipUnless(REDIS_URL, SKIP_REDIS)
class RealBrokerTests(unittest.TestCase):
    """A real Redis broker. The queue name is unique per test so nothing else on the machine is touched."""

    def setUp(self):
        import redis
        self.world = World()
        from workqueue.store import MemoryQueueStore
        self.store = MemoryQueueStore()
        self.q = build(self.store, self.world, dict(FLAGS))
        self.queue = 'wq-test-' + uuid.uuid4().hex[:10]
        self.app = tasks.make_app(REDIS_URL, runtime=self.q.rt)
        self.app.conf.task_default_queue = self.queue
        self.redis = redis.Redis.from_url(REDIS_URL)
        self.redis.delete(self.queue)

    def tearDown(self):
        self.redis.delete(self.queue)
        self.redis.close()
        self.world.close()

    def test_publishing_puts_one_message_per_claim_on_the_broker_and_clears_the_markers(self):
        self.q.intake.submit(good_claim('A')); self.q.intake.submit(good_claim('G'))
        report = relay.sweep(self.store, tasks.publisher(self.app), self.world.clock())
        self.assertEqual(report['published'], 2)
        self.assertEqual(self.redis.llen(self.queue), 2)
        self.assertEqual(self.store.pending_outbox(10), [])
        self.assertEqual(self.store.get('A')['state'], 'triaged')          # nothing consumed yet: delivery is separate from processing

    def test_a_worker_consuming_from_the_broker_takes_the_claims_to_ready(self):
        from celery.contrib.testing.worker import start_worker
        self.q.intake.submit(good_claim('A')); self.q.intake.submit(good_claim('G'))
        relay.sweep(self.store, tasks.publisher(self.app), self.world.clock())
        with start_worker(self.app, pool='solo', perform_ping_check=False, queues=[self.queue], loglevel='ERROR', shutdown_timeout=20):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and self.store.counts().get('triaged|A|decide_high', 0) + self.store.counts().get('triaged|green|decide', 0):
                time.sleep(0.2)
        self.assertEqual(self.store.counts(), {'ready|A|decide_high': 1, 'ready|green|decide': 1})
        self.assertEqual(self.redis.llen(self.queue), 0)


if __name__ == '__main__':
    unittest.main()
