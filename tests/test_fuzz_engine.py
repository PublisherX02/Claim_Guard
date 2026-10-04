"""Differential fuzzing: the engine and the independent oracle must agree on every claim Hypothesis can build."""
import logging
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
import oracle
from claim_gen import random_claim
from engine_core import config, validate_transport
from hypothesis import assume, given, strategies as st
from test_stress_differential import mutate, statuses
from yara_engine import evaluate

CFG = config(ROOT)
PACK = oracle.load_rules_pack(ROOT)
ALLOWED = {'PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE'}


def setUpModule():
    logging.disable(logging.WARNING)  # the engine logs each isolated rule crash; junk input makes thousands


def tearDownModule():
    logging.disable(logging.NOTSET)


class EngineFuzz(unittest.TestCase):
    @given(fs.claim_seeds, st.integers(0, 3))
    def test_engine_equals_oracle_on_mutated_claims(self, seed, rounds):
        rng = random.Random(seed)
        claim = random_claim(rng)
        for _ in range(rounds):
            claim = mutate(claim, rng)
        try:
            validate_transport(claim)
        except Exception:
            assume(False)  # ingestion quarantines it; the engine's fail-safe answer there is covered by the junk-field test
        errors = []
        got = statuses(evaluate(claim, CFG, errors))
        self.assertEqual(errors, [], 'a rule crashed and was isolated')
        self.assertEqual(got, oracle.evaluate(claim, PACK))
        self.assertEqual(len(got), 15)
        self.assertTrue(set(got.values()) <= ALLOWED)

    @given(fs.claim_seeds, st.sampled_from(sorted(fs.HOSTILE_STRINGS)))
    def test_hostile_text_in_free_text_fields_changes_no_verdict(self, seed, text):
        rng = random.Random(seed)
        claim = random_claim(rng)
        want = statuses(evaluate(claim, CFG))
        claim['notes'] = text
        for att in claim.get('attachments') or []:
            if isinstance(att, dict):
                att['text'] = text
        self.assertEqual(statuses(evaluate(claim, CFG)), want)

    @given(fs.claim_seeds, st.sampled_from(['patient_id', 'currency', 'policy_id', 'coverage', 'lines', 'authorizations', 'attachments', 'total_amount']), fs.json_values)
    def test_engine_never_raises_when_a_field_holds_arbitrary_json(self, seed, field, junk):
        claim = random_claim(random.Random(seed))
        claim[field] = junk
        errors = []
        results = evaluate(claim, CFG, errors)
        self.assertEqual(len(results), 15)
        self.assertTrue({r['status'] for r in results} <= ALLOWED)
        crashed = {e.split(':')[0] for e in errors}
        for r in results:  # fail closed: a rule that crashed on garbage must never read as a pass
            if r['rule_id'] in crashed:
                self.assertEqual(r['status'], 'UNABLE_TO_ASSESS', r['rule_id'])


if __name__ == '__main__':
    unittest.main()
