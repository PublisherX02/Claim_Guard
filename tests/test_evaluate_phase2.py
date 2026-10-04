import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
import eval_sets as es
import evaluate_phase2 as ev
from engine_core import config
from eval_metrics import severity_groups


class ScoreSetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cfg = config(ROOT)
        cls.sev = severity_groups(cls.cfg['rules'])

    def test_the_stress_split_scores_perfectly_and_baselines_do_not(self):
        s3 = es.organizer_sets(ROOT)[2]
        r = ev.score_set(s3, self.cfg, self.sev, baselines=True)
        self.assertEqual(r['summary']['claims'], 50)
        self.assertEqual(r['summary']['results'], 750)
        self.assertEqual(r['summary']['disagreements']['count'], 0)
        self.assertEqual(r['engine_crashes'], 0)
        self.assertEqual(r['summary']['overall']['f1'], 1.0)
        self.assertEqual(r['baselines']['always_pass']['overall']['f1'], 0.0)
        self.assertLess(r['baselines']['starter_baseline']['overall']['f1'], 0.9)
        self.assertIn('macro_severity', r['summary'])

    def test_a_wrong_label_shows_up_as_a_disagreement(self):
        s3 = es.organizer_sets(ROOT)[2]
        real = s3.items

        def flipped():
            for n, (c, g) in enumerate(real()):
                if n == 0:
                    g = dict(g, R001='FAIL' if g['R001'] != 'FAIL' else 'PASS')
                yield c, g
        s3.items = flipped
        r = ev.score_set(s3, self.cfg, self.sev)
        self.assertEqual(r['summary']['disagreements']['count'], 1)

    def test_an_isolated_rule_crash_is_counted_not_skipped(self):
        import logging
        import yara_engine
        original = yara_engine.DETAIL_FUNCS['R007']
        yara_engine.DETAIL_FUNCS['R007'] = lambda claim, cfg: 1 / 0
        logging.disable(logging.WARNING)
        try:
            r = ev.score_set(es.organizer_sets(ROOT)[2], self.cfg, self.sev)
        finally:
            yara_engine.DETAIL_FUNCS['R007'] = original
            logging.disable(logging.NOTSET)
        self.assertEqual(r['engine_crashes'], 50)
        self.assertGreater(r['summary']['disagreements']['count'], 0)

    def test_boundary_set_scores_only_the_named_rules(self):
        r = ev.score_set(es.boundary_set(ROOT), self.cfg, self.sev)
        self.assertEqual(r['summary']['results'], 140)
        self.assertEqual(r['summary']['disagreements']['count'], 0)


class RecordedAiStepTests(unittest.TestCase):
    def write(self, tmp, text):
        d = Path(tmp) / 'outputs' / 'defense'
        d.mkdir(parents=True)
        (d / 'load.json').write_text(text, encoding='utf-8')

    def test_reads_the_recorded_numbers(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.write(tmp, json.dumps({'ai_step_seconds': {'median': 1.0, 'p95': 2.0, 'calls': 3}}))
            self.assertEqual(ev.recorded_ai_step(tmp), {'median': 1.0, 'p95': 2.0, 'calls': 3})

    def test_a_missing_file_a_missing_key_and_bad_json_all_give_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(ev.recorded_ai_step(tmp))
        for text in ('{}', '{"ai_step_seconds": null}', 'not json', '[]'):
            with tempfile.TemporaryDirectory() as tmp:
                self.write(tmp, text)
                self.assertIsNone(ev.recorded_ai_step(tmp), text)


class RunAllTests(unittest.TestCase):
    def test_small_run_writes_both_files_with_every_set_cited(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = ev.run_all(ROOT, tmp, generated=60, mutant_attempts=60, repeats=1, audited_claims=3)
            metrics = json.loads((Path(tmp) / 'metrics.json').read_text(encoding='utf-8'))
            prov = json.loads((Path(tmp) / 'provenance.json').read_text(encoding='utf-8'))
            self.assertEqual(sorted(metrics['sets']), ['S1', 'S2', 'S3', 'S4', 'S5', 'S6', 'S7', 'S8', 'S9'])
            self.assertEqual(sorted(e['set_id'] for e in prov['sets']), sorted(metrics['sets']))
            for e in prov['sets']:
                self.assertEqual(e['claims'], metrics['sets'][e['set_id']]['summary']['claims'])
            self.assertEqual(metrics['commit'], prov['commit'])
            for key in ('engine_per_claim', 'ingest_fhir_per_claim', 'ingest_csv_per_claim', 'audited_template_per_claim'):
                self.assertGreater(metrics['latency'][key]['n'], 0)
            self.assertIn('median', metrics['ai_step_recorded']['seconds'])
            self.assertEqual(m['commit'], metrics['commit'])

    def test_accuracy_sections_are_deterministic(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            x = ev.run_all(ROOT, a, generated=40, mutant_attempts=40, repeats=1, audited_claims=2)
            y = ev.run_all(ROOT, b, generated=40, mutant_attempts=40, repeats=1, audited_claims=2)
            self.assertEqual({k: v['summary'] for k, v in x['sets'].items()},
                             {k: v['summary'] for k, v in y['sets'].items()})

    def test_does_not_touch_the_committed_evidence_directory(self):
        before = (ROOT / 'outputs' / 'evaluation').exists()
        with tempfile.TemporaryDirectory() as tmp:
            ev.run_all(ROOT, tmp, generated=10, mutant_attempts=10, repeats=1, audited_claims=1)
        self.assertEqual((ROOT / 'outputs' / 'evaluation').exists(), before)


if __name__ == '__main__':
    unittest.main()
