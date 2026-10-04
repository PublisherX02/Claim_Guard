import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
import access_experiments as ex

TINY_PLAN = {'login': {1: 2, 4: 4}, 'read': {1: 6, 4: 12}}


class ExperimentTests(unittest.TestCase):
    def test_the_smoke_run_passes_every_check_against_a_real_server(self):
        result = ex.smoke(rounds=4)
        failed = [c['name'] for c in result['checks'] if not c['ok']]
        self.assertEqual(failed, [])
        self.assertEqual(result['passed'], result['total'])
        self.assertGreaterEqual(result['total'], 20)

    def test_a_failing_check_is_reported_as_failed(self):
        checks = ex.Checks()
        checks.check('good', True)
        checks.check('bad', False, 'detail text')
        out = checks.summary()
        self.assertEqual((out['passed'], out['total']), (1, 2))
        self.assertEqual([c['name'] for c in out['checks'] if not c['ok']], ['bad'])

    def test_the_load_run_reports_latency_and_throughput_for_every_level_and_no_errors(self):
        result = ex.load(rounds=4, plan=TINY_PLAN)
        for kind in ('login', 'read'):
            self.assertEqual(sorted(result[kind]), ['1', '4'])
            for level, row in result[kind].items():
                self.assertEqual(row['errors'], 0, (kind, level))
                self.assertGreater(row['n'], 0)
                self.assertTrue(0 < row['p50_ms'] <= row['p95_ms'] <= row['p99_ms'])
                self.assertGreater(row['throughput_per_s'], 0)
        self.assertEqual(result['bcrypt_rounds'], 4)

    def test_main_writes_the_evidence_file_where_asked_and_nowhere_else(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'access.json'
            before = (ROOT / 'outputs' / 'defense' / 'access.json').exists()
            code = ex.main(['all', '--scale', 'tiny', '--rounds', '4', '--out', str(out)])
            self.assertEqual(code, 0)
            data = json.loads(out.read_text(encoding='utf-8'))
            self.assertEqual(set(data), {'environment', 'store', 'smoke', 'load'})
            self.assertEqual(data['smoke']['passed'], data['smoke']['total'])
            self.assertTrue(data['environment']['commit'])
            self.assertEqual((ROOT / 'outputs' / 'defense' / 'access.json').exists(), before)


if __name__ == '__main__':
    unittest.main()
