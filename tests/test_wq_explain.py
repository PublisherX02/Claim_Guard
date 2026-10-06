"""The AI explanation step: it adds words and never holds a claim up. Fake clock, fake model that fails, slows and recovers."""
import dataclasses
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import routing_config as rc
from workqueue.breaker import CircuitBreaker, Fatal, Timeout, Transient
from workqueue.explain import ExplainStep, backoff, failure_shape, fill, template_key
from workqueue.intake import Intake
from wq_world import FakeEngine, World, good_claim, store_makers

SENTINEL = 'PAT-SENTINEL-0042'
FLAGS = {'F1': {'R001': ('FAIL', 'high')}, 'F2': {'R001': ('FAIL', 'high')}, 'F3': {'R002': ('FAIL', 'medium')},
         'TWO': {'R001': ('FAIL', 'high'), 'R002': ('FAIL', 'medium')}, 'OK': {}}


def engine_with_evidence(claim):
    rows = FakeEngine(FLAGS)(claim)
    for r in rows:
        if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
            r['evidence'] = [{'path': '/patient_id', 'value': claim['patient_id']}]
            r['affected_line_ids'] = ['L1']
    return rows


class FakeModel:
    def __init__(self, clock):
        self.clock, self.calls, self.requests, self.script = clock, 0, [], []
        self.text = 'The value {value} on line {line} breaks this rule.'

    def __call__(self, request, deadline):
        self.calls += 1
        self.requests.append((dict(request), deadline))
        step = self.script.pop(0) if self.script else 'ok'
        if step == 'ok':
            return self.text
        if step == 'transient':
            raise Transient('503')
        if step == 'timeout':
            raise Timeout('slow')
        if step == 'fatal':
            raise Fatal('400')
        if step == 'slow':
            self.clock.advance(100)
            return self.text
        if step == 'bug':
            raise KeyError('model adapter bug')
        return step                                      # any other value is returned as the model's answer


class StepBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.clock = self.world.clock
        self.intake = Intake(self.store, engine_with_evidence, self.world.log, self.clock, 'p', 'e')
        self.model = FakeModel(self.clock)
        self.guard_calls = []
        self.sleeps = []
        self.cfg = rc.DEFAULT
        self.breaker = CircuitBreaker(self.clock)
        self.step = self.make_step()

    def make_step(self, **kw):
        def guard(text, result):
            self.guard_calls.append(text)
            return 'FRAUD' not in text
        args = dict(sleep=self.sleeps.append, max_attempts=3, prompt_version='1.6.0', model_name='m1', deadline_seconds=90.0)
        args.update(kw)
        return ExplainStep(self.store, self.model, guard, self.breaker, self.clock, random.Random(5), lambda: self.cfg,
                           lambda r: f"deterministic {r['rule_id']}", **args)

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def submit(self, claim_id):
        self.intake.submit(good_claim(claim_id, patient=SENTINEL))
        return self.store.get(claim_id)

    def explanation(self, claim_id):
        return self.store.get(claim_id)['explanation']

    def cache_dump(self):
        raise NotImplementedError

    # ---- the happy path and the cache
    def test_success_moves_to_explained_fills_the_values_and_caches_a_template(self):
        self.submit('F1')
        self.assertEqual(self.step.run('F1', 1), 'explained')
        d = self.store.get('F1')
        self.assertEqual((d['state'], d['explanation']['outcome']), ('explained', 'ai_used'))
        self.assertEqual(d['explanation']['findings']['R001'], {'text': f'The value {SENTINEL} on line L1 breaks this rule.', 'source': 'ai'})
        self.assertEqual(d['events'][-1]['actor'], 'system:explain')
        self.assertEqual(d['events'][-1]['detail'], {'outcome': 'ai_used', 'findings': 1, 'skipped': 0})
        self.assertEqual(self.model.calls, 1)

    def test_the_model_request_and_the_cache_never_contain_a_claim_value(self):
        self.submit('F1')
        self.step.run('F1', 1)
        for request, _ in self.model.requests:
            self.assertNotIn(SENTINEL, repr(request))
            self.assertEqual(sorted(request), ['failure_shape', 'placeholders', 'prompt_version', 'rule_id'])
        key = template_key('R001', failure_shape({'status': 'FAIL', 'severity': 'high', 'affected_line_ids': ['L1']}), '1.6.0', 'm1')
        template = self.store.cache_get(key)
        self.assertEqual(template, 'The value {value} on line {line} breaks this rule.')
        self.assertNotIn(SENTINEL, key + template)
        self.assertNotIn(SENTINEL, self.cache_dump())

    def test_a_second_identical_failure_shape_is_a_cache_hit_with_no_model_call(self):
        self.submit('F1'); self.submit('F2')
        self.step.run('F1', 1)
        self.assertEqual(self.step.run('F2', 1), 'explained')
        self.assertEqual(self.model.calls, 1)
        d = self.store.get('F2')
        self.assertEqual((d['explanation']['outcome'], d['explanation']['findings']['R001']['source']), ('cache_hit', 'cache'))

    def test_the_cache_is_keyed_by_rule_shape_prompt_version_and_model(self):
        keys = {template_key('R001', 'FAIL/high/1', '1.6.0', 'm1'), template_key('R002', 'FAIL/high/1', '1.6.0', 'm1'),
                template_key('R001', 'FAIL/medium/1', '1.6.0', 'm1'), template_key('R001', 'FAIL/high/n', '1.6.0', 'm1'),
                template_key('R001', 'FAIL/high/1', '1.7.0', 'm1'), template_key('R001', 'FAIL/high/1', '1.6.0', 'm2')}
        self.assertEqual(len(keys), 6)
        self.submit('F1'); self.step.run('F1', 1)
        self.submit('F3')                                           # another rule: a miss
        self.step.run('F3', 1)
        self.assertEqual(self.model.calls, 2)
        other = self.make_step(prompt_version='9.9.9')
        self.submit('F2')
        other.run('F2', 1)                                          # same rule and shape but another prompt version: a miss
        self.assertEqual(self.model.calls, 3)

    def test_the_guard_runs_on_the_filled_text_even_for_a_cached_template(self):
        self.submit('F1'); self.submit('F2')
        self.step.run('F1', 1)
        self.step.run('F2', 1)
        self.assertEqual(len(self.guard_calls), 2)
        self.assertTrue(all(SENTINEL in text for text in self.guard_calls))

    def test_text_the_guard_refuses_is_not_cached_and_the_claim_keeps_the_deterministic_text(self):
        self.model.text = 'This looks like FRAUD at {value}.'
        self.submit('F1')
        self.assertEqual(self.step.run('F1', 1), 'explanation_skipped')
        d = self.store.get('F1')
        self.assertEqual((d['explanation']['outcome'], d['explanation']['findings']['R001']),
                         ('skipped_guard', {'text': 'deterministic R001', 'source': 'template'}))
        key = template_key('R001', 'FAIL/high/1', '1.6.0', 'm1')
        self.assertIsNone(self.store.cache_get(key))

    def test_a_cached_template_that_fails_the_guard_for_these_values_is_not_used(self):
        key = template_key('R001', 'FAIL/high/1', '1.6.0', 'm1')
        self.store.cache_put(key, 'Escalate as FRAUD {value}')
        self.submit('F1')
        self.assertEqual(self.step.run('F1', 1), 'explanation_skipped')
        self.assertEqual(self.model.calls, 0)
        self.assertEqual(self.explanation('F1')['outcome'], 'skipped_guard')

    # ---- everything that can go wrong still ends in a skipped explanation, never an error
    def skipped(self, claim_id, label, calls=None):
        self.submit(claim_id)
        self.assertEqual(self.step.run(claim_id, 1), 'explanation_skipped')
        d = self.store.get(claim_id)
        self.assertEqual(d['explanation']['outcome'], label)
        self.assertEqual(d['explanation']['findings']['R001'], {'text': 'deterministic R001', 'source': 'template'})
        self.assertEqual(d['state'], 'explanation_skipped')
        if calls is not None:
            self.assertEqual(self.model.calls, calls)

    def test_budget_exhausted(self):
        self.cfg = dataclasses.replace(rc.DEFAULT, ai_daily_budget=0)
        self.skipped('F1', 'skipped_budget', calls=0)

    def test_rate_exceeded(self):
        self.cfg = dataclasses.replace(rc.DEFAULT, ai_per_minute=0)
        self.skipped('F1', 'skipped_rate', calls=0)

    def test_the_rate_limit_resets_with_the_minute(self):
        self.cfg = dataclasses.replace(rc.DEFAULT, ai_per_minute=1)
        self.clock.t = 6000.0
        self.submit('F1'); self.submit('F3')
        self.step.run('F1', 1)
        self.assertEqual(self.step.run('F3', 1), 'explanation_skipped')
        self.clock.advance(60)
        self.submit('TWO')
        self.assertEqual(self.step.run('TWO', 1), 'explained')               # a new minute: R001 is cached, R002 gets its one call
        self.assertEqual(self.model.calls, 2)

    def test_breaker_open(self):
        for _ in range(5):
            with self.assertRaises(Transient):
                self.breaker.call(lambda: (_ for _ in ()).throw(Transient('x')))
        self.skipped('F1', 'skipped_breaker', calls=0)

    def test_a_model_that_answers_after_the_ninety_second_ceiling(self):
        self.model.script = ['slow']
        self.skipped('F1', 'skipped_timeout', calls=1)
        self.assertEqual(self.model.requests[0][1], 1000.0 + 90.0)           # the deadline is passed to the model, not enforced by a kill

    def test_a_model_that_keeps_timing_out(self):
        self.model.script = ['timeout', 'timeout', 'timeout']
        self.skipped('F1', 'skipped_timeout', calls=3)

    def test_a_fatal_model_error_is_not_retried(self):
        self.model.script = ['fatal', 'ok']
        self.skipped('F1', 'skipped_error', calls=1)
        self.assertEqual(self.sleeps, [])

    def test_an_unexpected_adapter_exception_is_contained(self):
        self.model.script = ['bug']
        self.skipped('F1', 'skipped_error', calls=1)

    def test_a_transient_failure_is_retried_with_jittered_backoff_and_then_succeeds(self):
        self.model.script = ['transient', 'transient', 'ok']
        self.submit('F1')
        self.assertEqual(self.step.run('F1', 1), 'explained')
        self.assertEqual(self.model.calls, 3)
        rng = random.Random(5)
        self.assertEqual(self.sleeps, [rng.uniform(0, 1.0), rng.uniform(0, 2.0)])

    def test_retries_stop_at_the_configured_attempts(self):
        self.model.script = ['transient'] * 10
        self.skipped('F1', 'skipped_error', calls=3)
        self.assertEqual(len(self.sleeps), 2)
        self.assertTrue(all(0 <= s <= 30 for s in self.sleeps))

    def test_backoff_is_capped_and_never_negative(self):
        rng = random.Random(1)
        for attempt in range(0, 12):
            for _ in range(20):
                self.assertTrue(0 <= backoff(attempt, rng=rng) <= min(30.0, 2 ** attempt))

    def test_a_model_that_returns_rubbish_gives_a_skipped_explanation(self):
        for i, bad in enumerate((None, 5, '', '   ', 'x' * 1600, ['a'], {'text': 'a'})):
            cid = f'G{i}'
            FLAGS[cid] = {'R001': ('FAIL', 'high')}
            self.addCleanup(FLAGS.pop, cid, None)
            self.model.script = [bad]
            self.skipped(cid, 'skipped_error')

    # ---- pipeline behaviour
    def test_green_claims_never_reach_the_model(self):
        self.submit('OK')
        self.assertEqual(self.step.run('OK', 1), 'noop')
        self.assertEqual((self.model.calls, self.store.get('OK')['state']), (0, 'triaged'))

    def test_running_twice_does_nothing_the_second_time(self):
        self.submit('F1')
        self.assertEqual(self.step.run('F1', 1), 'explained')
        self.assertEqual(self.step.run('F1', 1), 'noop')
        self.assertEqual(self.model.calls, 1)
        self.assertEqual(sum(1 for e in self.store.get('F1')['events'] if e['to'] == 'explained'), 1)

    def test_an_unknown_claim_is_a_noop(self):
        self.assertEqual(self.step.run('NOPE', 1), 'noop')

    def test_two_findings_one_limited_gives_a_partial_explanation_that_is_still_skipped(self):
        self.cfg = dataclasses.replace(rc.DEFAULT, ai_per_minute=1)
        self.submit('TWO')
        self.assertEqual(self.step.run('TWO', 1), 'explanation_skipped')
        f = self.explanation('TWO')['findings']
        self.assertEqual({k: v['source'] for k, v in f.items()}, {'R001': 'ai', 'R002': 'template'})
        self.assertEqual(self.explanation('TWO')['outcome'], 'skipped_rate')

    def test_a_timeout_on_the_first_finding_skips_the_rest_without_more_calls(self):
        self.model.script = ['slow']
        self.submit('TWO')
        self.step.run('TWO', 1)
        self.assertEqual(self.model.calls, 1)
        self.assertEqual({v['source'] for v in self.explanation('TWO')['findings'].values()}, {'template'})

    def test_a_non_green_claim_with_nothing_flagged_is_skipped_with_a_label(self):
        self.store.put_triaged({'claim_id': 'D1', 'version': 1, 'input_hash': 'x', 'claim': good_claim('D1'), 'results': [],
                                'receipt': {'lane': 'B', 'eligibility': 'decide_high', 'score': 0, 'created_at': 1.0}})
        self.assertEqual(self.step.run('D1', 1), 'explanation_skipped')
        self.assertEqual(self.explanation('D1')['outcome'], 'skipped_none')
        self.assertEqual(self.model.calls, 0)


class FillTests(unittest.TestCase):
    def test_values_are_substituted_in_one_pass(self):
        result = {'evidence': [{'path': '/x', 'value': '{line}'}], 'affected_line_ids': ['L9']}
        self.assertEqual(fill('v={value} l={line}', result), 'v={line} l=L9')

    def test_missing_evidence_and_lines_give_empty_strings(self):
        self.assertEqual(fill('v={value} l={line}', {}), 'v= l=')

    def test_a_long_value_is_cut(self):
        self.assertEqual(len(fill('{value}', {'evidence': [{'value': 'a' * 5000}]})), 200)

    def test_other_braces_are_left_alone(self):
        self.assertEqual(fill('{other} {0} {value}', {'evidence': [{'value': 'v'}]}), '{other} {0} v')


def cache_dump_memory(store):
    return repr(store._cache)


def cache_dump_mongo(store):
    return repr(list(store.raw_db['cache'].find({}, {'_id': 0})))


def _make(name, factory, dump):
    return type('Step_' + name, (StepBase, unittest.TestCase), {'factory': staticmethod(factory), 'cache_dump': lambda self: dump(self.store)})


for _name, _factory in store_makers():
    globals()['Step_' + _name] = _make(_name, _factory, cache_dump_memory if _name == 'memory' else cache_dump_mongo)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
