import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import eval_metrics as m


def claim(tally, cid, gold, pred):
    tally.add_claim(cid, gold, pred)


class PrfTests(unittest.TestCase):
    def test_hand_checked_values(self):
        r = m.prf({'tp': 1, 'fp': 1, 'fn': 1, 'tn': 1})
        self.assertEqual((r['precision'], r['recall'], r['f1']), (0.5, 0.5, 0.5))

    def test_no_positive_anywhere_is_undefined_not_zero(self):
        r = m.prf({'tp': 0, 'fp': 0, 'fn': 0, 'tn': 9})
        self.assertIsNone(r['f1'])
        self.assertIsNone(r['precision'])
        self.assertIsNone(r['recall'])

    def test_only_false_alarms_is_a_real_zero(self):
        r = m.prf({'tp': 0, 'fp': 3, 'fn': 0, 'tn': 9})
        self.assertEqual(r['f1'], 0.0)
        self.assertEqual(r['precision'], 0.0)
        self.assertIsNone(r['recall'])


class MacroTests(unittest.TestCase):
    def test_undefined_values_are_excluded_and_listed(self):
        r = m.macro({'a': 1.0, 'b': 0.0, 'c': None})
        self.assertEqual(r['macro_f1'], 0.5)
        self.assertEqual(r['defined'], 2)
        self.assertEqual(r['excluded'], ['c'])

    def test_all_undefined(self):
        r = m.macro({'a': None})
        self.assertIsNone(r['macro_f1'])
        self.assertEqual(r['defined'], 0)


class StructureTests(unittest.TestCase):
    def test_categories_partition_the_fifteen_rules(self):
        flat = [r for rules in m.CATEGORIES.values() for r in rules]
        self.assertEqual(sorted(flat), list(m.RULES))
        self.assertEqual(len(flat), len(set(flat)))

    def test_severity_groups(self):
        g = m.severity_groups([{'rule_id': 'R001', 'severity': 'high'}, {'rule_id': 'R002', 'severity': 'medium'}])
        self.assertEqual(g, {'high': ('R001',), 'medium': ('R002',)})


class BoundTests(unittest.TestCase):
    def test_zero_errors_closed_form(self):
        self.assertAlmostEqual(m.clopper_pearson_upper(0, 100), 1 - 0.05 ** (1 / 100), places=9)

    def test_one_error_in_100(self):
        self.assertAlmostEqual(m.clopper_pearson_upper(1, 100), 0.0466, places=3)

    def test_monotone_and_above_the_point_estimate(self):
        a, b = m.clopper_pearson_upper(2, 200), m.clopper_pearson_upper(3, 200)
        self.assertLess(a, b)
        self.assertGreater(a, 2 / 200)

    def test_all_errors_and_empty(self):
        self.assertEqual(m.clopper_pearson_upper(5, 5), 1.0)
        self.assertIsNone(m.clopper_pearson_upper(0, 0))

    def test_large_k_uses_wilson_and_stays_a_probability(self):
        u = m.clopper_pearson_upper(5000, 100000)
        self.assertTrue(0.05 < u < 0.06)


class LatencyTests(unittest.TestCase):
    def test_nearest_rank_percentiles(self):
        v = list(range(1, 101))
        self.assertEqual((m.percentile(v, 50), m.percentile(v, 95), m.percentile(v, 99)), (50, 95, 99))
        self.assertIsNone(m.percentile([], 50))

    def test_summary(self):
        s = m.latency_summary([1.0, 2.0, 3.0])
        self.assertEqual((s['n'], s['mean'], s['max']), (3, 2.0, 3.0))


G = {'R001': 'FAIL', 'R002': 'PASS'}


class TallyTests(unittest.TestCase):
    def build(self):
        t = m.Tally()
        # c1: gold FAIL/PASS, engine FAIL/FAIL  -> R001 tp, R002 fp
        claim(t, 'c1', {'R001': 'FAIL', 'R002': 'PASS'}, {'R001': 'FAIL', 'R002': 'FAIL'})
        # c2: gold FAIL/PASS, engine PASS/PASS  -> R001 fn, R002 tn
        claim(t, 'c2', {'R001': 'FAIL', 'R002': 'PASS'}, {'R001': 'PASS', 'R002': 'PASS'})
        return t

    def test_per_rule_counts_and_f1(self):
        s = self.build().summary()
        r1, r2 = s['per_rule']['R001'], s['per_rule']['R002']
        self.assertEqual((r1['tp'], r1['fn'], r1['f1']), (1, 1, 2 / 3))
        self.assertEqual((r2['fp'], r2['tn'], r2['f1']), (1, 1, 0.0))

    def test_category_pools_the_counts_of_its_rules(self):
        s = self.build().summary()
        # only R001 and R002 were scored, so the groups hold what was scored
        self.assertEqual(s['by_category']['completeness_and_arithmetic']['tp'], 1)
        self.assertEqual(s['by_category']['timing']['fp'], 1)

    def test_disagreements_are_counted_and_sampled(self):
        s = self.build().summary()
        self.assertEqual(s['disagreements']['count'], 2)
        self.assertEqual(s['disagreements']['examples'][0]['claim_id'], 'c1')

    def test_status_accuracy_and_multiclass(self):
        s = self.build().summary()
        self.assertEqual(s['status_accuracy'], 0.5)
        self.assertIn('macro_f1', s['multiclass'])

    def test_missing_rule_in_prediction_is_an_error(self):
        t = m.Tally()
        with self.assertRaises(KeyError):
            t.add_claim('c', G, {'R001': 'FAIL'})

    def test_valid_claim_rates_need_all_fifteen_rules(self):
        t = m.Tally()
        ok = {r: 'PASS' for r in m.RULES}
        bad = dict(ok, R005='FAIL')
        unable = dict(ok, R009='UNABLE_TO_ASSESS')
        # clean claim, engine raises a FAIL -> false positive
        t.add_claim('c1', ok, bad)
        # clean claim, engine abstains -> false abstention
        t.add_claim('c2', ok, unable)
        # claim with an UNABLE in gold but no FAIL: valid-without-FAIL, not clean; engine quiet
        t.add_claim('c3', unable, unable)
        # claim with a FAIL in gold: not a valid claim
        t.add_claim('c4', bad, bad)
        v = t.summary()['valid_claims']
        self.assertEqual((v['claims_without_fail']['n'], v['claims_without_fail']['k']), (3, 1))
        self.assertEqual((v['clean_claims']['n'], v['clean_claims']['k']), (2, 1))
        self.assertEqual(v['clean_claim_false_abstention']['k'], 1)
        self.assertEqual(v['clean_result_false_alarm']['k'], 1)
        self.assertEqual(v['clean_result_false_alarm']['n'], 30)

    def test_partial_claims_are_excluded_from_valid_claim_rates(self):
        t = m.Tally()
        t.add_claim('c1', {'R001': 'PASS'}, {'R001': 'PASS'})
        v = t.summary()['valid_claims']
        self.assertEqual(v['claims_without_fail']['n'], 0)
        self.assertIsNone(v['claims_without_fail']['rate'])


if __name__ == '__main__':
    unittest.main()
