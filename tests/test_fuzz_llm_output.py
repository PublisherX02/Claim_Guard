"""The model may only explain. Fuzz what it can say: any reply, any shape, any text."""
import copy
import logging
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
from hypothesis import given, strategies as st
from engine_core import config, load_jsonl
from llm_adapter import MockExplanationProvider, explain_with_fallback, validate_explanation
from yara_engine import evaluate

CFG = config(ROOT)
CLAIM = next(c for c in load_jsonl(ROOT / 'data/stress/claims.jsonl')
             if any(r['status'] == 'FAIL' for r in evaluate(c, CFG)))
FINDING = next(r for r in evaluate(CLAIM, CFG) if r['status'] == 'FAIL')
RULE = next(r for r in CFG['rules'] if r['rule_id'] == FINDING['rule_id'])
FALLBACK = MockExplanationProvider()


def setUpModule():
    logging.disable(logging.WARNING)  # every rejected reply logs a fallback warning; thousands of them drown the result


def tearDownModule():
    logging.disable(logging.NOTSET)


class Replies:
    def __init__(self, reply):
        self.reply = reply

    def explain(self, finding, rule, untrusted_note=None):
        return copy.deepcopy(self.reply)


def valid_reply(**over):
    r = {'explanation': FINDING['explanation'], 'cited_evidence_paths': [FINDING['evidence'][0]['path']],
         'cited_rule_ids': [FINDING['rule_id']], 'needs_human_review': True}
    r.update(over)
    return r


reply_shapes = st.one_of(
    fs.json_values,
    st.fixed_dictionaries({'explanation': fs.hostile_text, 'cited_evidence_paths': st.lists(fs.hostile_text, max_size=3),
                           'cited_rule_ids': st.lists(fs.hostile_text, max_size=2),
                           'needs_human_review': st.one_of(st.booleans(), st.integers(0, 2), st.text(max_size=5))}),
    st.builds(lambda t: valid_reply(explanation=t), fs.hostile_text),
    st.builds(lambda extra: dict(valid_reply(), **extra), st.dictionaries(st.text(min_size=1, max_size=8), fs.json_values, max_size=3)),
)


class LlmOutputFuzz(unittest.TestCase):
    @given(reply_shapes)
    def test_any_reply_is_a_schema_valid_dict_or_the_template(self, reply):
        before = copy.deepcopy(FINDING)
        out, used_fallback, error, _ = explain_with_fallback(Replies(reply), FALLBACK, copy.deepcopy(FINDING), copy.deepcopy(RULE))
        validate_explanation(out, FINDING)                       # whatever came back obeys the per-finding schema
        self.assertEqual(FINDING, before)                         # the deterministic finding is untouched
        self.assertTrue(out['needs_human_review'] is True)        # a FAIL always stays flagged for a human
        if used_fallback:
            self.assertIsNotNone(error)

    @given(fs.hostile_text)
    def test_model_text_never_changes_the_engine_verdict(self, text):
        want = evaluate(CLAIM, CFG)
        explain_with_fallback(Replies(valid_reply(explanation=text)), FALLBACK, copy.deepcopy(FINDING), copy.deepcopy(RULE), untrusted_note=text)
        self.assertEqual(evaluate(CLAIM, CFG), want)

    @given(st.sampled_from([Exception('x'), TimeoutError(), ConnectionError(), RuntimeError('boom'), RecursionError(), MemoryError()]))
    def test_provider_exceptions_fall_back(self, exc):
        class Boom:
            def explain(self, *a, **k):
                raise exc
        out, used_fallback, _, _ = explain_with_fallback(Boom(), FALLBACK, copy.deepcopy(FINDING), copy.deepcopy(RULE))
        self.assertTrue(used_fallback)
        validate_explanation(out, FINDING)


if __name__ == '__main__':
    unittest.main()
