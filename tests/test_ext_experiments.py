import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_experiments as xp


class ExperimentTests(unittest.TestCase):
    def test_every_constructed_scenario_gets_the_status_fixed_by_its_construction(self):
        result = xp.scenarios()
        wrong = [c for c in result['cases'] if not c['ok']]
        self.assertEqual(wrong, [])
        self.assertEqual(result['agree'], result['total'])
        covered = {c['rule'] for c in result['cases']}
        self.assertEqual(covered, set(xp.RULES))
        for rule in xp.RULES:
            self.assertGreaterEqual(len([c for c in result['cases'] if c['rule'] == rule]), 3, rule)

    def test_main_writes_the_evidence_where_asked_and_nowhere_else(self):
        official = ROOT / 'outputs' / 'evaluation' / 'extensions.json'
        before = official.read_bytes() if official.exists() else None
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'x.json'
            self.assertEqual(xp.main(['--scale', 'tiny', '--out', str(out)]), 0)
            data = json.loads(out.read_text(encoding='utf-8'))
        self.assertEqual(set(data), {'environment', 'public_claims_status_counts', 'oracle_agreement_on_public_claims', 'scenarios', 'timing', 'note'})
        self.assertTrue(data['environment']['commit'])
        self.assertEqual(len(data['environment']['pack_hash']), 64)
        self.assertEqual(set(data['public_claims_status_counts']), set(xp.RULES))
        self.assertEqual(data['oracle_agreement_on_public_claims']['agree'], data['oracle_agreement_on_public_claims']['compared'])
        self.assertEqual(official.read_bytes() if official.exists() else None, before)

    def test_single_claim_rules_never_fire_on_public_claims_by_design(self):
        counts, _ = xp.fire_counts(xp.public_claims()[:60])
        for rid in ('E001', 'E002', 'E004'):
            self.assertEqual(counts[rid].get('FAIL', 0), 0, rid)


if __name__ == '__main__':
    unittest.main()
