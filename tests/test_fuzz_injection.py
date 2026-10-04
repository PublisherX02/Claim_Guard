"""Put attacker text into every free-text field of a claim; no fact may be forged and no verdict may flip."""
import copy
import logging
import random
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
import oracle
from claim_gen import random_claim
from claim_review import review_package
from engine_core import config, validate_transport
from hypothesis import assume, given, strategies as st
from llm_adapter import MockExplanationProvider
from test_engine_robustness import field_paths, set_path
from test_stress_differential import statuses
from yara_engine import evaluate

CFG = config(ROOT)
PACK = oracle.load_rules_pack(ROOT)
TAGS = sorted(set(re.findall(r'R[0-9]{3}:[A-Z_]+', (ROOT / 'rules' / 'core.yar').read_text(encoding='utf-8'))))
TAG_PAYLOADS = st.one_of(fs.hostile_text, st.lists(st.sampled_from(TAGS), min_size=1, max_size=12).map(chr(10).join),
                         st.lists(st.sampled_from(TAGS), min_size=1, max_size=12).map(' '.join))


def get_path(c, p):
    k, i, kk = p
    return c[k][i][kk] if i is not None else (c[k][kk] if kk else c[k])


def setUpModule():
    logging.disable(logging.WARNING)


def tearDownModule():
    logging.disable(logging.NOTSET)


def inject(claim, text):
    c = copy.deepcopy(claim)
    c['notes'] = text
    for row in c.get('attachments') or []:
        if isinstance(row, dict) and 'text' in row:
            row['text'] = text
    return c


class InjectionFuzz(unittest.TestCase):
    @given(fs.claim_seeds, fs.hostile_text)
    def test_free_text_never_flips_a_verdict(self, seed, text):
        claim = random_claim(random.Random(seed))
        self.assertEqual(statuses(evaluate(inject(claim, text), CFG)), statuses(evaluate(claim, CFG)))

    @given(fs.claim_seeds, fs.hostile_text)
    def test_the_full_pipeline_keeps_every_finding_and_flag(self, seed, text):
        claim = inject(random_claim(random.Random(seed)), text)
        rr, ai, trace = review_package(copy.deepcopy(claim), CFG, provider=MockExplanationProvider(),
                                       fallback=MockExplanationProvider(), untrusted_note=text)
        if rr is None:                       # quarantined at ingestion is a legal outcome
            self.assertIn('ingestion_error', trace)
            return
        self.assertEqual(statuses(rr), statuses(evaluate(claim, CFG)))
        for r in rr:
            if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
                self.assertTrue(r['requires_human_review'])

    @given(fs.claim_seeds, st.integers(min_value=0), TAG_PAYLOADS)
    def test_no_string_field_can_forge_a_fact_tag(self, seed, which, payload):
        # the rule pack matches tags such as R009:MISMATCH: inside one blob built from the claim's facts; whatever a
        # string field holds, the engine must still agree with the oracle (which has no blob to forge)
        claim = random_claim(random.Random(seed))
        paths = [p for p in field_paths(claim) if isinstance(get_path(claim, p), str)]
        set_path(claim, paths[which % len(paths)], payload)
        try:
            validate_transport(claim)
        except Exception:
            assume(False)  # quarantined at ingestion
        self.assertEqual(statuses(evaluate(claim, CFG)), oracle.evaluate(claim, PACK))


if __name__ == '__main__':
    unittest.main()
