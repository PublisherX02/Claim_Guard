"""What the system does with no model configured, and the guard and deterministic text it falls back on."""
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import routing_config as rc
from workqueue.breaker import CircuitBreaker
from workqueue.explain import ExplainStep, default_guard, deterministic_text, no_model
from workqueue.intake import Intake
from workqueue.store import MemoryQueueStore
from wq_world import FakeEngine, World, good_claim

RESULT = {'claim_id': 'C1', 'rule_id': 'R001', 'status': 'FAIL', 'severity': 'high', 'explanation': 'Quantity 5 exceeds the limit 3.',
          'evidence': [{'path': '/lines/0/quantity', 'value': 5}], 'affected_line_ids': ['L1']}


class DefaultsTests(unittest.TestCase):
    def test_the_default_guard_accepts_grounded_text_and_refuses_what_the_existing_checks_refuse(self):
        self.assertTrue(default_guard('The quantity 5 on line L1 is above the limit of 3.', RESULT))
        self.assertFalse(default_guard('This looks like fraud on line L1.', RESULT))
        self.assertFalse(default_guard('The claim is approved for payment.', RESULT))
        self.assertFalse(default_guard('The amount is $5 too high.', RESULT))
        self.assertFalse(default_guard('a' * 40, RESULT))

    def test_the_deterministic_text_is_the_engines_explanation_or_a_plain_fallback(self):
        self.assertEqual(deterministic_text(RESULT), 'Quantity 5 exceeds the limit 3.')
        for bad in ({'rule_id': 'R001', 'status': 'FAIL'}, {'rule_id': 'R001', 'status': 'FAIL', 'explanation': '  '},
                    {'rule_id': 'R001', 'status': 'FAIL', 'explanation': 5}):
            self.assertEqual(deterministic_text(bad), 'R001: FAIL')

    def test_with_no_model_every_claim_keeps_the_engines_text_and_the_breaker_never_opens(self):
        world = World()
        try:
            store, clock = MemoryQueueStore(), world.clock
            breaker = CircuitBreaker(clock)
            flags = {f'N{i}': {'R001': ('FAIL', 'high'), 'R002': ('FAIL', 'medium')} for i in range(12)}
            step = ExplainStep(store, no_model, lambda t, r: True, breaker, clock, random.Random(1), lambda: rc.DEFAULT,
                               lambda r: f"deterministic {r['rule_id']}")
            intake = Intake(store, FakeEngine(flags), world.log, clock, 'p', 'e')
            for i in range(12):
                intake.submit(good_claim(f'N{i}'))
                self.assertEqual(step.run(f'N{i}', 1), 'explanation_skipped')
                d = store.get(f'N{i}')
                self.assertEqual(d['explanation']['outcome'], 'skipped_no_model')
                self.assertEqual({v['source'] for v in d['explanation']['findings'].values()}, {'template'})
            self.assertEqual(breaker.state, 'closed')
        finally:
            world.close()


if __name__ == '__main__':
    unittest.main()
