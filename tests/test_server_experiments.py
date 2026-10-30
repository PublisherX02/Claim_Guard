"""The server experiment suite (experiments/server/, docs/65): its helpers and the report logic, without a server."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'experiments' / 'server'))

import common  # noqa: E402
import make_server_report as rep  # noqa: E402


class Helpers(unittest.TestCase):
    def test_totp_matches_the_rfc_6238_vector(self):
        seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ'  # ASCII "12345678901234567890"
        self.assertEqual(common.totp(seed, at=59, digits=8), '94287082')
        self.assertEqual(common.totp(seed, at=1111111109, digits=8), '07081804')
        self.assertEqual(common.totp(seed, at=59), '287082')

    def test_totp_accepts_a_seed_with_spaces_and_lowercase(self):
        self.assertEqual(common.totp('gezd gnbv gy3t qojq gezd gnbv gy3t qojq', at=59), '287082')

    def test_percentiles(self):
        self.assertEqual(common.pct([1, 2, 3, 4, 5], 50), 3)
        self.assertEqual(common.pct([1, 2, 3, 4, 5], 100), 5)
        self.assertIsNone(common.pct([], 50))
        self.assertEqual(common.summary([]), {'n': 0})
        self.assertEqual(common.summary([2, 4])['mean'], 3)

    def test_docker_sizes(self):
        self.assertEqual(common.parse_size('1GiB'), 1024 ** 3)
        self.assertEqual(common.parse_size('512MiB'), 512 * 1024 ** 2)
        self.assertEqual(common.parse_size('2GB'), 2_000_000_000)
        self.assertEqual(common.parse_size('100B'), 100)

    def test_fresh_claims_are_new_work_and_leave_the_original_alone(self):
        claim = {'claim_id': 'CG-AAAAAAAAAAAA', 'invoice_number': 'INV-CG-AAAAAAAAAAAA', 'coverage': {'coverage_id': 'COV-CG-AAAAAAAAAAAA'}, 'lines': [{'line_id': 'L1'}]}
        a, b = common.fresh_claim(claim), common.fresh_claim(claim)
        self.assertNotEqual(a['claim_id'], b['claim_id'])
        self.assertRegex(a['claim_id'], r'^CG-[0-9A-F]{12}$')
        self.assertEqual(a['invoice_number'], 'INV-' + a['claim_id'])
        self.assertEqual(a['coverage']['coverage_id'], 'COV-' + a['claim_id'])
        self.assertEqual(claim['claim_id'], 'CG-AAAAAAAAAAAA')
        self.assertEqual(a['lines'], claim['lines'])

    def test_the_api_refuses_to_start_without_credentials(self):
        import os
        saved = {k: os.environ.pop(k, None) for k in ('CG_URL', 'CG_BADGE', 'CG_PASSWORD', 'CG_TOTP_SEED')}
        try:
            with self.assertRaises(SystemExit):
                common.Api()
        finally:
            os.environ.update({k: v for k, v in saved.items() if v is not None})

    def test_the_stress_pack_loads(self):
        self.assertGreaterEqual(len(common.load_claims(limit=5)), 5)


def sample(t, mem, limit, gpu_mib=None):
    return {'t': t, 'gpu': {'mem_used_mib': gpu_mib, 'mem_total_mib': 10240.0, 'util_pct': 50.0, 'temp_c': 60.0, 'power_w': 200.0} if gpu_mib else None,
            'containers': [{'name': 'api', 'mem_bytes': mem, 'mem_limit_bytes': limit, 'cpu_pct': 10.0}, {'name': 'embed', 'mem_bytes': mem * 2, 'mem_limit_bytes': limit * 4, 'cpu_pct': 5.0}],
            'host': {'mem_total': 12 * 1024 ** 3, 'mem_available': 6 * 1024 ** 3}}


class ReportLogic(unittest.TestCase):
    def test_resource_facts_find_peaks_and_the_growth_rate(self):
        mib = 1024 ** 2
        # total memory grows by 3 * 10 MiB every 60 s during the span of s02 (10 minutes) = 1800 MiB an hour
        samples = [sample(1000 + i * 60, (100 + i * 10) * mib, 1024 ** 3, gpu_mib=4000 + i) for i in range(12)]
        log = [{'experiment': 's02', 'started_at': 1000, 'seconds': 660}]
        f = rep.resource_facts(samples, log)
        self.assertEqual(f['peaks']['api'], 210 * mib)
        self.assertEqual(f['gpu']['mem_peak_mib'], 4011)
        self.assertAlmostEqual(f['growth_mib_per_hour'], 1800, delta=1)
        self.assertEqual(f['host_total'], 12 * 1024 ** 3)

    def test_no_samples_is_not_a_crash(self):
        self.assertEqual(rep.resource_facts([], []), {'n': 0})

    def _results(self, best, dry=False):
        stage = {'batch_size': 50, 'batches': 2, 'wall_s': 10, 'claims_ok': 100, 'errors': [], 'error_count': 0, 'claims_per_minute': best, 'accept_s': {'p50': 0.1},
                 'done_s': {'p50': 1.0, 'p95': 1.5}, 'per_claim_ms': {'p50': 20.0, 'p95': 30.0}}
        return {'env': {'host': {'hostname': 'testbox', 'mem_total_bytes': 12 * 1024 ** 3, 'gpu': 'RTX 3080, 10240 MiB'}, 'started': 'x', 'dry_run': dry},
                's02_pipeline': {'stages': [stage], 'soak': [], 'dry_run': dry,
                                'responsive_before': {'dashboard': {'p50': 0.01, 'p95': 0.02}}, 'responsive_after': {'dashboard': {'p50': 0.01, 'p95': 0.05}},
                                'checks': {'best_claims_per_minute': best, 'reaches_200_per_minute': best >= 200, 'no_errors': True, 'still_responsive_after_load': True}}}

    def test_the_scoreboard_marks_pass_and_fail_against_the_target(self):
        rows = {r[1]: r[4] for r in rep.scoreboard(self._results(250), {})}
        self.assertEqual(rows['Claims per minute through the whole pipeline (best stage)'], 'PASS')
        rows = {r[1]: r[4] for r in rep.scoreboard(self._results(120), {})}
        self.assertEqual(rows['Claims per minute through the whole pipeline (best stage)'], 'FAIL')

    def test_memory_over_the_budget_fails(self):
        f = {'peak_total': 11 * 1024 ** 3, 'host_total': 12 * 1024 ** 3, 'peaks': {'api': 1}, 'limits': {'api': 1 << 40}}
        rows = {r[1]: r[4] for r in rep.scoreboard({}, f)}
        self.assertEqual(rows['Peak memory of all containers vs server memory'], 'FAIL')

    def test_a_dry_run_report_carries_the_banner_and_a_real_one_does_not(self):
        text = rep.build('t', self._results(250, dry=True), [], {}, {}, {'log': []})
        self.assertIn('DRY RUN', text)
        text = rep.build('t', self._results(250), [], {}, {}, {'log': []})
        self.assertNotIn('DRY RUN', text)
        self.assertIn('testbox', text)

    def test_an_experiment_that_did_not_run_is_said_so_and_never_invented(self):
        text = rep.build('t', self._results(250), [], {}, {}, {'log': [{'experiment': 's04', 'skipped': 'needs --docs <folder>'}]})
        self.assertIn('Not run in this session', text)
        self.assertIn('s04', text)
        self.assertNotIn('S4. Documents', text)

    def test_the_report_builds_from_files_on_disk_without_figures(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = rep.OUT_ROOT
            rep.OUT_ROOT = Path(tmp)
            try:
                d = Path(tmp) / 'r1'
                d.mkdir()
                for k, v in self._results(250).items():
                    (d / ('env.json' if k == 'env' else k + '.json')).write_text(json.dumps(v), encoding='utf-8')
                run_dir, data, samples = rep.load('r1')
                self.assertIn('s02_pipeline', data)
                self.assertEqual(samples, [])
                text = rep.build('r1', data, samples, {}, rep.resource_facts(samples, []), None)
                self.assertIn('2. Scoreboard', text)
            finally:
                rep.OUT_ROOT = old

    def test_sections_are_numbered_in_order_whatever_ran(self):
        text = rep.renumber(chr(10).join(['## 3. A', 'text', '## 6. B', '## 10. C']))
        self.assertEqual([x for x in text.split(chr(10)) if x.startswith('##')], ['## 1. A', '## 2. B', '## 3. C'])

    def test_a_dry_run_never_overwrites_the_results_chapter_or_the_real_figures(self):
        import subprocess
        doc = ROOT / 'docs' / '66_Server_Experiment_Results.md'
        before = doc.read_text(encoding='utf-8')
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp) / 'dry1'
            d.mkdir()
            for k, v in self._results(250, dry=True).items():
                (d / ('env.json' if k == 'env' else k + '.json')).write_text(json.dumps(v), encoding='utf-8')
            env = dict(__import__('os').environ, PYTHONPATH=str(ROOT / 'experiments' / 'server'))
            code = "import sys; import common, make_server_report as r; from pathlib import Path; r.OUT_ROOT = Path(%r); sys.argv = ['x', '--run', 'dry1', '--no-figures']; r.main()" % tmp
            done = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True, env=env, cwd=str(ROOT))
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertIn('DRY RUN', (d / 'report.md').read_text(encoding='utf-8'))
        self.assertEqual(doc.read_text(encoding='utf-8'), before)


class Packing(unittest.TestCase):
    def test_secret_looking_text_is_found_and_clean_text_is_not(self):
        import pack_results as pk
        self.assertTrue(pk.findings('JWT_SECRET=abcdefghijklmnop'))
        self.assertTrue(pk.findings('-----BEGIN ' + 'PRIVATE KEY-----'))      # built in two pieces: the repo's secret scan rejects the literal header in a tracked file
        self.assertTrue(pk.findings('token ' + 'eyJ' + 'hbGciOiJIUzI1NiJ9' + '.eyJ' + 'zdWIiOiIxMjM0NTY3ODkwIn0' + '.abcdefghijklmnopqrstuv'))      # pieces, so no secret scanner takes a test input for a real token
        self.assertFalse(pk.findings('{"claims_per_minute": 1943.6, "claim_id": "CG-27BFD8541DEB"}'))

    def test_a_run_with_a_credential_in_it_is_not_packed(self):
        import os
        import pack_results as pk
        with tempfile.TemporaryDirectory() as tmp:
            old, pk.OUT_ROOT = pk.OUT_ROOT, Path(tmp)
            try:
                d = Path(tmp) / 'r'
                d.mkdir()
                (d / 'a.json').write_text('{"ok": true}', encoding='utf-8')
                sys.argv = ['x', '--run', 'r']
                pk.main()
                self.assertTrue((Path(tmp) / 'r.zip').exists())
                os.environ['CG_PASSWORD'] = 'hunter2-hunter2'
                (d / 'b.json').write_text('{"leak": "hunter2-hunter2"}', encoding='utf-8')
                with self.assertRaises(SystemExit):
                    pk.main()
            finally:
                os.environ.pop('CG_PASSWORD', None)
                pk.OUT_ROOT = old


class Scripts(unittest.TestCase):
    def test_every_script_compiles_and_has_a_docstring_with_its_usage(self):
        import ast
        for p in (ROOT / 'experiments' / 'server').glob('*.py'):
            tree = ast.parse(p.read_text(encoding='utf-8'))
            self.assertTrue(ast.get_docstring(tree), p.name)

    def test_the_restart_experiment_refuses_to_run_without_confirmation(self):
        import subprocess
        done = subprocess.run([sys.executable, str(ROOT / 'experiments' / 'server' / 's05_resilience.py'), '--run', 'x'], capture_output=True, text=True)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn('--yes-restart', done.stderr + done.stdout)


if __name__ == '__main__':
    unittest.main()
