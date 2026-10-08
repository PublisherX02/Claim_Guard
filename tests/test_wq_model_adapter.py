"""The production model adapter (src/workqueue/model_adapter.py): no claim value leaves the process, every call has a deadline, errors map
onto the breaker's classes, and the optional second pass can fix a format but never change a word. Offline: the model is a fake."""
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from engine_core import config, load_jsonl
from workqueue import breaker as brk
from workqueue.breaker import CircuitBreaker
from workqueue.explain import ExplainStep, deterministic_text, default_guard
from workqueue.model_adapter import PROMPT_VERSION, TemplateModel, from_provider
from yara_engine import evaluate
from wq_world import FakeClock

CFG = config(ROOT)
RULES = {r['rule_id']: r for r in CFG['rules']}
CLAIMS = load_jsonl(ROOT / 'data' / 'development' / 'claims.jsonl')
REQ = {'rule_id': 'R001', 'failure_shape': 'FAIL/high/1', 'prompt_version': PROMPT_VERSION, 'placeholders': ['{value}', '{line}']}
GOOD = 'Rule R001 failed because {value} on line {line} is missing. Request the missing source information from the provider.'


class Script:
    """A fake model: pops an answer (text or exception) per call and records every prompt and timeout it was given."""

    def __init__(self, *answers):
        self.answers, self.prompts, self.timeouts = list(answers), [], []

    def __call__(self, prompt, timeout):
        self.prompts.append(prompt)
        self.timeouts.append(timeout)
        a = self.answers.pop(0)
        if isinstance(a, BaseException):
            raise a
        return a


def leaves(o):
    if isinstance(o, dict):
        for v in o.values():
            yield from leaves(v)
    elif isinstance(o, list):
        for v in o:
            yield from leaves(v)
    elif o is not None:
        yield str(o)


class NoClaimValueLeaves(unittest.TestCase):
    def test_no_value_from_a_real_claim_appears_in_any_outbound_prompt(self):
        checked = 0
        for claim in CLAIMS[:60]:
            secret = {v for v in leaves(claim) if len(v) >= 4 and v not in ('active', 'SAR', 'Synthetic', 'imaging-report')}
            # only values that are specific to this claim: ids, codes with digits, dates, long text
            secret = {v for v in secret if any(ch.isdigit() for ch in v) or len(v) > 25}
            for result in evaluate(claim, CFG):
                if result['status'] == 'PASS':
                    continue
                from workqueue.explain import failure_shape
                request = {'rule_id': result['rule_id'], 'failure_shape': failure_shape(result),
                           'prompt_version': PROMPT_VERSION, 'placeholders': ['{value}', '{line}']}
                for two in (False, True):
                    fake = Script(GOOD, '{"template": "%s"}' % GOOD)
                    TemplateModel(fake, RULES, two_pass=two)(request, deadline=10 ** 9)
                    for prompt in fake.prompts:
                        leaked = [v for v in secret if v in prompt]
                        self.assertEqual(leaked, [], f"{result['rule_id']} two_pass={two}")
                    checked += len(fake.prompts)
        self.assertGreater(checked, 50)

    def test_the_request_the_step_builds_carries_no_claim_data(self):
        seen = []
        step = ExplainStep(None, lambda req, dl: seen.append(req) or GOOD, default_guard, CircuitBreaker(FakeClock()), FakeClock(),
                           random.Random(1), None, deterministic_text)
        result = {'rule_id': 'R001', 'status': 'FAIL', 'severity': 'high', 'affected_line_ids': ['L1'],
                  'evidence': [{'path': '/patient_id', 'value': 'PAT-SECRET-77'}]}
        step._ask(result, deadline=10 ** 9)
        self.assertNotIn('PAT-SECRET-77', repr(seen))
        self.assertEqual(set(seen[0]), {'rule_id', 'failure_shape', 'prompt_version', 'placeholders'})


class Deadline(unittest.TestCase):
    def test_the_timeout_is_the_time_left_and_never_more(self):
        clock = FakeClock(100.0)
        fake = Script(GOOD)
        TemplateModel(fake, RULES, clock=clock)(REQ, deadline=190.0)
        self.assertAlmostEqual(fake.timeouts[0], 90.0)

    def test_two_passes_share_one_deadline(self):
        clock = FakeClock(0.0)

        def slow(prompt, timeout):
            clock.advance(40)
            return GOOD if 'Rule (from the rulebook)' in prompt else '{"template": "%s"}' % GOOD
        m = TemplateModel(slow, RULES, two_pass=True, clock=clock)
        # 40 s per call against a 70 s ceiling: pass 1 gets 70 s, pass 2 gets the 30 s left (clock now 40), and finishes at 80.
        self.assertEqual(m(REQ, deadline=70.0), GOOD)
        self.assertEqual(clock(), 80.0)
        with self.assertRaises(brk.Timeout):  # a second claim sharing that ceiling has nothing left
            m(REQ, deadline=70.0)

    def test_a_call_that_starts_with_no_time_left_is_refused_without_calling_the_model(self):
        fake = Script(GOOD)
        with self.assertRaises(brk.Timeout):
            TemplateModel(fake, RULES, clock=FakeClock(100.0))(REQ, deadline=100.0)
        self.assertEqual(fake.prompts, [])


class ErrorMapping(unittest.TestCase):
    def raises(self, exc):
        with self.assertRaises(Exception) as cm:
            TemplateModel(Script(exc), RULES)(REQ, deadline=10 ** 9)
        return cm.exception

    def test_timeouts_rate_limits_connection_and_server_errors_are_transient(self):
        class APITimeoutError(Exception):
            pass

        class RateLimitError(Exception):
            status_code = 429

        class APIConnectionError(Exception):
            pass

        class Boom(Exception):
            status_code = 503
        self.assertIsInstance(self.raises(APITimeoutError('t')), brk.Timeout)
        for exc in (RateLimitError('r'), APIConnectionError('c'), Boom('s')):
            e = self.raises(exc)
            self.assertIsInstance(e, brk.Transient)
            self.assertNotIsInstance(e, brk.Timeout)

    def test_rejected_requests_and_unknown_errors_are_fatal(self):
        class BadRequestError(Exception):
            status_code = 400

        class AuthenticationError(Exception):
            status_code = 401
        for exc in (BadRequestError('b'), AuthenticationError('a'), KeyError('k')):
            self.assertIsInstance(self.raises(exc), brk.Fatal)

    def test_an_unknown_rule_is_fatal(self):
        with self.assertRaises(brk.Fatal):
            TemplateModel(Script(GOOD), RULES)(dict(REQ, rule_id='R999'), deadline=10 ** 9)

    def test_empty_or_non_text_answers_are_transient(self):
        for bad in ('', '   ', None, 5):
            with self.subTest(answer=bad):
                self.assertIsInstance(self.raises_for(bad), brk.Transient)

    def raises_for(self, answer):
        with self.assertRaises(brk.Transient) as cm:
            TemplateModel(Script(answer), RULES)(REQ, deadline=10 ** 9)
        return cm.exception

    def test_braces_other_than_the_two_placeholders_are_rejected(self):
        for bad in ('Rule R001 failed for {patient}.', 'Rule {0} failed.', 'Stray } brace.', 'x' * 1600):
            with self.subTest(answer=bad[:20]):
                self.raises_for(bad)


class TwoPass(unittest.TestCase):
    def test_one_pass_makes_one_call_and_two_pass_makes_two(self):
        one, two = Script(GOOD), Script(GOOD, '{"template": "%s"}' % GOOD)
        self.assertEqual(TemplateModel(one, RULES)(REQ, 10 ** 9), GOOD)
        self.assertEqual(TemplateModel(two, RULES, two_pass=True)(REQ, 10 ** 9), GOOD)
        self.assertEqual((len(one.prompts), len(two.prompts)), (1, 2))

    def test_pass_two_may_change_whitespace_but_not_a_word(self):
        spaced = '{"template": "%s"}' % GOOD.replace('. ', '.\\n  ')
        self.assertEqual(TemplateModel(Script(GOOD, spaced), RULES, two_pass=True)(REQ, 10 ** 9), GOOD)
        for bad in ('{"template": "Rule R001 failed. The claim is approved."}', '{"template": ""}', 'not json',
                    '{"template": "%s", "extra": 1}' % GOOD, '{"template": 5}', '[1]'):
            with self.subTest(answer=bad[:30]):
                with self.assertRaises(brk.Transient):
                    TemplateModel(Script(GOOD, bad), RULES, two_pass=True)(REQ, 10 ** 9)

    def test_pass_two_sees_only_the_pass_one_text(self):
        fake = Script(GOOD, '{"template": "%s"}' % GOOD)
        TemplateModel(fake, RULES, two_pass=True)(REQ, 10 ** 9)
        self.assertIn(GOOD, fake.prompts[1])
        self.assertNotIn('Rule (from the rulebook)', fake.prompts[1])

    def test_the_two_modes_have_different_cache_identities(self):
        a, b = TemplateModel(Script(), RULES, name='m'), TemplateModel(Script(), RULES, name='m', two_pass=True)
        self.assertNotEqual((a.name, a.prompt_version), (b.name, b.prompt_version))


class AgainstTheStep(unittest.TestCase):
    def test_the_step_fills_and_guards_what_the_adapter_returns(self):
        step = ExplainStep(None, TemplateModel(Script(GOOD), RULES), default_guard, CircuitBreaker(FakeClock()), FakeClock(),
                           random.Random(1), None, deterministic_text)
        text, why = step._ask({'rule_id': 'R001', 'status': 'FAIL', 'severity': 'high', 'affected_line_ids': ['L1']}, 10 ** 9)
        self.assertEqual((text, why), (GOOD, None))

    def test_fatal_and_open_breaker_become_skips_not_exceptions(self):
        clock = FakeClock()
        fatal = ExplainStep(None, TemplateModel(Script(RuntimeError('x')), RULES), default_guard, CircuitBreaker(clock), clock,
                            random.Random(1), None, deterministic_text)
        self.assertEqual(fatal._ask({'rule_id': 'R001', 'status': 'FAIL', 'severity': 'high'}, 10 ** 9), (None, 'skipped_error'))


class ProviderWrapper(unittest.TestCase):
    def test_from_provider_scopes_the_timeout_to_a_copy_and_leaves_the_shared_client_alone(self):
        class Client:
            def __init__(self, timeout=None):
                self.timeout = timeout

            def with_options(self, timeout):
                return Client(timeout)

        class Provider:
            model = 'm'

            def __init__(self):
                self.client = Client()
                self.seen = []

            def _complete(self, prompt):
                self.seen.append(self.client.timeout)
                return GOOD
        prov = Provider()
        model = from_provider(prov, RULES, clock=FakeClock(0.0))
        self.assertEqual(model(REQ, deadline=30.0), GOOD)
        self.assertIsNone(prov.client.timeout)  # the shared client was not changed
        self.assertEqual(prov.seen, [30.0])      # the call itself ran with the time left
        self.assertEqual(model.name, 'm')


class EnvWiring(unittest.TestCase):
    def test_nothing_set_means_no_model(self):
        from workqueue.model_adapter import model_from_env
        self.assertIsNone(model_from_env({}, RULES))
        self.assertIsNone(model_from_env({'QUEUE_AI_PROVIDER': '  '}, RULES))

    def test_ollama_address_comes_from_the_environment(self):
        from workqueue.model_adapter import model_from_env
        model = model_from_env({'QUEUE_AI_PROVIDER': 'ollama', 'OLLAMA_BASE_URL': 'http://ollama:11434/'}, RULES)
        self.assertIsNotNone(model)
        provider = model.complete.__closure__[0].cell_contents            # from_provider's closure holds the provider
        self.assertEqual(str(provider.client.base_url).rstrip('/'), 'http://ollama:11434/v1')

    def test_unknown_provider_is_a_configuration_error(self):
        from workqueue.model_adapter import model_from_env
        with self.assertRaises(ValueError):
            model_from_env({'QUEUE_AI_PROVIDER': 'bogus'}, RULES)

    def test_a_known_provider_without_its_key_fails_loudly_at_startup(self):
        import os
        from unittest import mock
        from workqueue.model_adapter import model_from_env
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop('FEATHERLESS_API_KEY', None)
            with mock.patch('llm_adapter._load_dotenv'):
                with self.assertRaises(RuntimeError):
                    model_from_env({'QUEUE_AI_PROVIDER': 'featherless'}, RULES)

    def test_a_configured_provider_builds_a_model_and_two_pass_is_opt_in(self):
        import os
        from unittest import mock
        from workqueue.model_adapter import model_from_env
        with mock.patch.dict(os.environ, {'FEATHERLESS_API_KEY': 'k' * 20}):
            one = model_from_env({'QUEUE_AI_PROVIDER': 'featherless'}, lambda: RULES)
            two = model_from_env({'QUEUE_AI_PROVIDER': 'featherless', 'QUEUE_AI_TWO_PASS': '1'}, RULES)
        self.assertFalse(one.two_pass)
        self.assertTrue(two.two_pass)


class ExperimentScript(unittest.TestCase):
    def setUp(self):
        sys.path.insert(0, str(ROOT / 'scripts'))
        import two_pass_experiment as tpe
        self.tpe = tpe

    def test_decision_rule_is_the_pre_registered_one(self):
        base = {'n': 40, 'guard_accepted': 0.80, 'failure_rate': 0.02, 'p95_seconds': 10.0}
        self.assertEqual(self.tpe.decide(base, dict(base, guard_accepted=0.86, p95_seconds=20.0)), 'adopt two-pass')
        self.assertEqual(self.tpe.decide(base, dict(base, guard_accepted=0.83)), 'keep one pass')            # under 5 points
        self.assertEqual(self.tpe.decide(base, dict(base, guard_accepted=0.90, failure_rate=0.06)), 'keep one pass')
        self.assertEqual(self.tpe.decide(base, dict(base, guard_accepted=0.90, p95_seconds=23.0)), 'keep one pass')
        self.assertIn('inconclusive', self.tpe.decide(dict(base, n=29), dict(base, n=29, guard_accepted=0.99)))

    def test_dry_run_writes_a_file_that_says_it_proves_nothing(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'tp.json'
            self.assertEqual(self.tpe.main(['--dry-run', '--claims', '12', '--out', str(out)]), 0)
            data = json.loads(out.read_text(encoding='utf-8'))
        self.assertTrue(data['dry_run'])
        self.assertIn('nothing about quality', data['note'])
        self.assertEqual(data['summary']['two_pass']['calls_per_finding'], 2.0)
        self.assertEqual(data['summary']['one_pass']['calls_per_finding'], 1.0)


if __name__ == '__main__':
    unittest.main()
