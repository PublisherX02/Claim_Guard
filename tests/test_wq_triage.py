"""Triage: the formula, the receipt, and agreement with the independent oracle on generated and real result sets."""
import copy
import sys
import unittest
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from hypothesis import HealthCheck, given, settings, strategies as st
from workqueue import routing_config as rc, triage
from oracle_routing import RULE_IDS, oracle

CFG = rc.DEFAULT


def results_of(**flags):
    """15 results, all PASS, then overridden: flags maps rule id to (status, severity)."""
    rows = [{'claim_id': 'C1', 'rule_id': rid, 'status': 'PASS', 'severity': 'high', 'affected_line_ids': [], 'evidence': []}
            for rid in RULE_IDS]
    for rid, (status, severity) in flags.items():
        for row in rows:
            if row['rule_id'] == rid:
                row.update(status=status, severity=severity)
    return rows


class FormulaTests(unittest.TestCase):
    def check(self, flags, score, lane, eligibility):
        r = results_of(**flags)
        self.assertEqual((triage.score(r, CFG), triage.lane(r, CFG), triage.eligibility(r)), (score, lane, eligibility), flags)

    def test_the_table_cases_written_out(self):
        self.check({}, 0, 'green', 'decide')
        self.check({'R001': ('FAIL', 'high')}, 4, 'A', 'decide_high')
        self.check({'R001': ('FAIL', 'high'), 'R002': ('FAIL', 'high'), 'R003': ('FAIL', 'medium')}, 10, 'B', 'decide_high')
        self.check({f'R00{i}': ('UNABLE_TO_ASSESS', 'medium') for i in range(1, 5)}, 4, 'B', 'decide')
        self.check({'R001': ('UNABLE_TO_ASSESS', 'medium')}, 1, 'A', 'decide')
        self.check({'R001': ('FAIL', 'medium'), 'R002': ('FAIL', 'medium')}, 4, 'A', 'decide')
        self.check({'R001': ('UNABLE_TO_ASSESS', 'high')}, 2, 'A', 'decide_high')

    def test_pass_and_not_applicable_never_count(self):
        self.check({'R001': ('NOT_APPLICABLE', 'high'), 'R002': ('PASS', 'high')}, 0, 'green', 'decide')

    def test_the_lane_thresholds_come_from_the_configuration(self):
        r = results_of(R001=('FAIL', 'high'), R002=('FAIL', 'high'))     # 8 points, 2 flagged
        self.assertEqual(triage.lane(r, CFG), 'A')
        import dataclasses
        self.assertEqual(triage.lane(r, dataclasses.replace(CFG, lane_b_score=8)), 'B')
        self.assertEqual(triage.lane(r, dataclasses.replace(CFG, lane_b_flagged=2)), 'B')

    def test_the_points_come_from_the_configuration(self):
        import dataclasses
        pts = copy.deepcopy(CFG.points); pts['FAIL']['high'] = 7
        self.assertEqual(triage.score(results_of(R001=('FAIL', 'high')), dataclasses.replace(CFG, points=pts)), 7)


class FailClosedTests(unittest.TestCase):
    """A result set that cannot be trusted is never green."""

    def assert_closed(self, results):
        self.assertEqual(triage.lane(results, CFG), 'B')
        self.assertEqual(triage.eligibility(results), 'decide_high')
        self.assertTrue(triage.degraded(results))

    def test_missing_short_long_and_garbage_result_sets(self):
        good = results_of()
        self.assertFalse(triage.degraded(good))
        for bad in ([], good[:14], good + [good[0]], None, 'x', {}, 15):
            self.assert_closed(bad)

    def test_duplicate_rule_id_unknown_status_unknown_severity_and_non_dict_items(self):
        dup = results_of(); dup[1]['rule_id'] = 'R001'
        status = results_of(); status[3]['status'] = 'MAYBE'
        severity = results_of(); severity[3]['severity'] = 'urgent'
        no_severity = results_of(); del no_severity[2]['severity']
        non_dict = results_of(); non_dict[5] = 'R006'
        none_item = results_of(); none_item[5] = None
        for bad in (dup, status, severity, no_severity, non_dict, none_item):
            self.assert_closed(bad)

    def test_unknown_severity_on_a_flagged_finding_counts_as_high(self):
        r = results_of(R004=('FAIL', 'urgent'))
        self.assertEqual(triage.score(r, CFG), 4)
        self.assertEqual(triage.eligibility(r), 'decide_high')


STATUS = st.sampled_from(['PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE'])
SEVERITY = st.sampled_from(['high', 'medium'])


@st.composite
def result_sets(draw):
    rows = results_of()
    for row in rows:
        row.update(status=draw(STATUS), severity=draw(SEVERITY))
    return rows


class OracleTests(unittest.TestCase):
    @settings(max_examples=300, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    @given(result_sets())
    def test_the_formula_equals_the_oracle_on_generated_result_sets(self, results):
        want = oracle(results)
        got = (triage.score(results, CFG), triage.lane(results, CFG), triage.eligibility(results), triage.degraded(results))
        self.assertEqual(got, want)

    @settings(max_examples=100, deadline=None)
    @given(result_sets(), st.integers(1, 15), st.integers(1, 40))
    def test_the_oracle_agrees_under_other_thresholds(self, results, flagged, score):
        import dataclasses
        cfg = dataclasses.replace(CFG, lane_b_flagged=flagged, lane_b_score=score)
        self.assertEqual(triage.lane(results, cfg), oracle(results, lane_b_flagged=flagged, lane_b_score=score)[1])

    def test_the_public_evaluation_sets_agree_with_the_oracle_and_the_lane_counts_are_shown(self):
        from engine_core import config, load_jsonl
        from yara_engine import evaluate
        cfg = config(str(ROOT))
        lanes = Counter()
        for split in ('development', 'validation', 'stress'):
            for claim in load_jsonl(str(ROOT / 'data' / split / 'claims.jsonl')):
                results = evaluate(claim, cfg, [])
                got = (triage.score(results, CFG), triage.lane(results, CFG), triage.eligibility(results), triage.degraded(results))
                self.assertEqual(got, oracle(results), claim['claim_id'])
                lanes[got[1]] += 1
        print('\nlane counts on the public sets:', dict(lanes))
        self.assertEqual(sum(lanes.values()), 600)


class ReceiptTests(unittest.TestCase):
    def receipt(self, claim, results, **kw):
        args = dict(cfg=CFG, rule_pack_hash='pack1', engine_version='1.0', now=1000.0)
        args.update(kw)
        return triage.make_receipt(claim, results, **args)

    def test_a_receipt_has_exactly_the_specified_fields(self):
        r = self.receipt({'claim_id': 'C1', 'a': 1}, results_of(R001=('FAIL', 'high')))
        self.assertEqual(set(r), {'claim_id', 'input_hash', 'rule_pack_hash', 'engine_version', 'facts_hash', 'result_hash',
                                  'statuses', 'score', 'lane', 'eligibility', 'config_version', 'degraded', 'created_at'})
        self.assertEqual((r['claim_id'], r['score'], r['lane'], r['eligibility'], r['config_version'], r['degraded'], r['created_at']),
                         ('C1', 4, 'A', 'decide_high', 1, False, 1000.0))
        self.assertEqual(r['statuses']['R001'], 'FAIL')
        self.assertEqual(len(r['statuses']), 15)

    def test_a_receipt_is_deterministic_and_sensitive_to_each_input(self):
        claim, results = {'claim_id': 'C1', 'a': 1}, results_of()
        base = self.receipt(claim, results)
        self.assertEqual(base, self.receipt(copy.deepcopy(claim), copy.deepcopy(results)))
        changed_status = self.receipt(claim, results_of(R002=('FAIL', 'high')))
        self.assertNotEqual(base['result_hash'], changed_status['result_hash'])
        self.assertNotEqual(base['input_hash'], self.receipt({'claim_id': 'C1', 'a': 2}, results)['input_hash'])
        evidence = results_of(); evidence[0]['evidence'] = [{'path': '/a', 'value': 1}]
        self.assertNotEqual(base['facts_hash'], self.receipt(claim, evidence)['facts_hash'])

    def test_hashes_do_not_depend_on_key_order_or_result_order(self):
        results = results_of(R001=('FAIL', 'high'))
        self.assertEqual(triage.input_hash({'b': 1, 'a': 2}), triage.input_hash({'a': 2, 'b': 1}))
        self.assertEqual(triage.result_hash(results), triage.result_hash(list(reversed(results))))

    def test_the_receipt_names_the_configuration_in_force(self):
        import dataclasses
        self.assertEqual(self.receipt({'claim_id': 'C1'}, results_of(), cfg=dataclasses.replace(CFG, version=5))['config_version'], 5)

    def test_a_degraded_set_gives_a_degraded_receipt_and_never_raises(self):
        r = self.receipt({'claim_id': 'C1'}, None)
        self.assertEqual((r['lane'], r['eligibility'], r['degraded']), ('B', 'decide_high', True))
        self.assertEqual(r['statuses'], {})

    def test_the_input_hash_refuses_data_that_is_not_json(self):
        with self.assertRaises(ValueError):
            triage.input_hash({'a': {1, 2}})


if __name__ == '__main__':
    unittest.main()
