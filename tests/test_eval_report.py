import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import render_eval_report as r

TINY_SUMMARY = {
    'claims': 2, 'results': 30, 'overall': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 29, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0},
    'per_rule': {'R001': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 1, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0}},
    'by_category': {'timing': {'tp': 0, 'fp': 0, 'fn': 0, 'tn': 4, 'precision': None, 'recall': None, 'f1': None, 'rules': ['R002']}},
    'macro_category': {'macro_f1': 1.0, 'defined': 1, 'excluded': ['timing']},
    'macro_rule': {'macro_f1': 1.0, 'defined': 1, 'excluded': []},
    'macro_severity': {'macro_f1': 1.0, 'defined': 1, 'excluded': []},
    'by_severity': {'high': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 0, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0, 'rules': ['R001']}},
    'status_accuracy': 1.0, 'multiclass': {'macro_f1': 1.0, 'defined': 2, 'excluded': [], 'per_status_f1': {}},
    'valid_claims': {k: {'k': 0, 'n': 5, 'rate': 0.0, 'upper95': 0.45} for k in
                     ('claims_without_fail', 'clean_claims', 'clean_claim_false_abstention', 'clean_result_false_alarm')},
    'disagreements': {'count': 0, 'examples': [], 'breakdown': []}, 'fail_error_rate': {'k': 0, 'n': 30, 'rate': 0.0, 'upper95': 0.09},
}
LAT = {'n': 1, 'mean': 1, 'p50': 1, 'p95': 1, 'p99': 1, 'max': 1}
METRICS = {'commit': 'abc', 'sets': {'S3': {'meta': {'tier': 'A', 'name': 'data/stress', 'path': 'engine', 'label_kind': 'organizer_key'},
                                           'summary': TINY_SUMMARY, 'engine_crashes': 0, 'engine_ms': LAT,
                                           'baselines': {'always_pass': TINY_SUMMARY, 'starter_baseline': TINY_SUMMARY}}},
           'latency': {'unit': 'milliseconds', 'environment': {'platform': 'x', 'python': '3.10', 'cpus': 1},
                       **{k: LAT for k in ('engine_per_claim', 'ingest_fhir_per_claim', 'ingest_csv_per_claim',
                                           'audited_template_per_claim')}},
           'ai_step_recorded': {'seconds': {'median': 2.86, 'p95': 28.89, 'calls': 117}, 'source': 'outputs/defense/load.json'}}
PROV = {'commit': 'abc', 'sets': [{'set_id': 'S3', 'tier': 'A', 'name': 'data/stress', 'label_kind': 'organizer_key',
                                   'label_source': 'k', 'generator': 'g', 'limitation': 'l', 'claims': 2, 'results': 30,
                                   'files': [{'path': 'data/stress/claims.jsonl', 'sha256': 'a' * 64}], 'notes': {},
                                   'commit': 'abc', 'path': 'engine'}]}


class RenderTests(unittest.TestCase):
    def test_undefined_values_print_as_na_not_zero(self):
        t = r.render_tables(METRICS, PROV)
        self.assertIn('n/a', t['categories'])

    def test_provenance_table_cites_the_hash_prefix(self):
        self.assertIn('aaaaaaaa', r.render_tables(METRICS, PROV)['provenance'])

    def test_every_table_is_produced_even_with_one_set(self):
        t = r.render_tables(METRICS, PROV)
        for name in ('provenance', 'tier_a', 'categories', 'per_rule', 'valid_claims', 'baselines', 'tier_b_c', 'latency', 'disagreements'):
            self.assertIn(name, t)

    def test_disagreement_table_lists_each_kind_with_its_count(self):
        import copy
        m = copy.deepcopy(METRICS)
        m['sets']['S3']['summary']['disagreements'] = {
            'count': 3, 'examples': [], 'breakdown': [{'rule_id': 'R009', 'gold': 'PASS', 'predicted': 'UNABLE_TO_ASSESS', 'count': 3}]}
        text = r.render_tables(m, PROV)['disagreements']
        self.assertIn('R009', text)
        self.assertIn('UNABLE_TO_ASSESS', text)
        self.assertIn('| 3 |', text)

    def test_no_disagreements_says_so(self):
        self.assertIn('No disagreements', r.render_tables(METRICS, PROV)['disagreements'])

    def test_apply_replaces_blocks_and_keeps_prose(self):
        doc = 'intro\n<!-- TABLE:latency -->\nold\n<!-- /TABLE:latency -->\noutro'
        out = r.apply_tables(doc, {'latency': 'NEW'})
        self.assertIn('NEW', out)
        self.assertNotIn('old', out)
        self.assertTrue(out.startswith('intro') and out.endswith('outro'))

    def test_missing_marker_is_an_error(self):
        with self.assertRaises(KeyError):
            r.apply_tables('no markers', {'latency': 'x'})


class CommittedReportTests(unittest.TestCase):
    def test_the_committed_report_matches_the_committed_evidence(self):
        metrics = json.loads((ROOT / 'outputs' / 'evaluation' / 'metrics.json').read_text(encoding='utf-8'))
        prov = json.loads((ROOT / 'outputs' / 'evaluation' / 'provenance.json').read_text(encoding='utf-8'))
        doc = (ROOT / 'docs' / '29_Test_Evaluation_Report.md').read_text(encoding='utf-8')
        self.assertEqual(r.apply_tables(doc, r.render_tables(metrics, prov)).replace('\r\n', '\n'), doc.replace('\r\n', '\n'))


if __name__ == '__main__':
    unittest.main()
