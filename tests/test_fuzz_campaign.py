import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_campaign
import fuzz_strategies as fs


class CampaignTests(unittest.TestCase):
    def test_run_surface_reports_status_and_time(self):
        r = fuzz_campaign.run_surface('test_fuzz_setup', examples=5)
        self.assertEqual(r['status'], 'pass')
        self.assertGreaterEqual(r['seconds'], 0)

    def test_a_failing_surface_is_reported_as_fail(self):
        r = fuzz_campaign.run_surface('test_does_not_exist', examples=5)
        self.assertEqual(r['status'], 'fail')

    def test_the_ci_profile_is_restored_afterwards(self):
        before = fs.ACTIVE
        fuzz_campaign.run_surface('test_fuzz_setup', examples=5)
        self.assertEqual(fs.ACTIVE, before)

    def test_main_writes_the_evidence_file_to_the_given_path_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'fuzz.json'
            code = fuzz_campaign.main(['--surfaces', 'test_fuzz_setup', '--examples', '5', '--out', str(out)])
            data = json.loads(out.read_text(encoding='utf-8'))
            self.assertEqual(code, 0)
            self.assertEqual(data['surfaces']['test_fuzz_setup']['status'], 'pass')


if __name__ == '__main__':
    unittest.main()
