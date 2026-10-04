import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import eval_sets as es
import oracle
from claim_gen import random_claim
from engine_core import config, validate_transport
from yara_engine import evaluate


class GeneratedSetTests(unittest.TestCase):
    def test_matches_the_status_coverage_stream(self):
        # the first kept claim must be the first claim status_coverage.py would keep for the same seed
        rng = random.Random(20260927)
        while True:
            c = random_claim(rng)
            try:
                validate_transport(c)
                break
            except Exception:
                continue
        s = es.generated_set(ROOT, n_claims=5, seed=20260927)
        first, _ = next(iter(s.items()))
        c['claim_id'] = 'CG-GEN-000000'
        self.assertEqual(first, c)

    def test_ids_unique_gold_from_oracle_and_notes(self):
        s = es.generated_set(ROOT, n_claims=300, seed=20260927)
        items = list(s.items())
        ids = [c['claim_id'] for c, _ in items]
        self.assertEqual(len(ids), 300)
        self.assertEqual(len(set(ids)), 300)
        pack = oracle.load_rules_pack(ROOT)
        claim, gold = items[0]
        self.assertEqual(gold, oracle.evaluate(claim, pack))
        self.assertEqual(s.notes['scored'], 300)
        self.assertGreaterEqual(s.notes['attempted'], 300)
        self.assertEqual((s.tier, s.label_kind), ('B', 'independent_oracle'))

    def test_renaming_does_not_change_any_verdict(self):
        cfg = config(ROOT)
        rng = random.Random(1)
        for _ in range(100):
            c = random_claim(rng)
            a = {r['rule_id']: r['status'] for r in evaluate(c, cfg)}
            b = {r['rule_id']: r['status'] for r in evaluate(dict(c, claim_id='CG-RENAMED'), cfg)}
            self.assertEqual(a, b)

    def test_deterministic(self):
        a = [c for c, _ in es.generated_set(ROOT, n_claims=20, seed=7).items()]
        b = [c for c, _ in es.generated_set(ROOT, n_claims=20, seed=7).items()]
        self.assertEqual(a, b)


class MutantSetTests(unittest.TestCase):
    def test_unique_ids_valid_transport_and_oracle_gold(self):
        s = es.mutant_set(ROOT, attempts=400, seed=20261004)
        items = list(s.items())
        self.assertGreater(len(items), 100)
        ids = [c['claim_id'] for c, _ in items]
        self.assertEqual(len(ids), len(set(ids)))
        pack = oracle.load_rules_pack(ROOT)
        for claim, gold in items[:50]:
            validate_transport(claim)
            self.assertEqual(gold, oracle.evaluate(claim, pack))
        self.assertEqual(s.notes['attempted'], 400)
        self.assertEqual(s.notes['scored'], len(items))

    def test_deterministic(self):
        a = [c for c, _ in es.mutant_set(ROOT, attempts=60, seed=3).items()]
        b = [c for c, _ in es.mutant_set(ROOT, attempts=60, seed=3).items()]
        self.assertEqual(a, b)


if __name__ == '__main__':
    unittest.main()
