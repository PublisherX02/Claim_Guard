"""The extension rules must leave the official fifteen exactly as they were."""
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import ext_baseline as eb

BASELINE = json.loads((ROOT / 'tests' / 'fixtures' / 'extension_baseline.json').read_text(encoding='utf-8'))


class OfficialEngineUntouchedTests(unittest.TestCase):
    def test_every_official_file_is_byte_for_byte_what_it_was_before_the_extension_work(self):
        for rel, digest in BASELINE['files'].items():
            self.assertEqual(eb.file_hash(rel), digest, rel)

    def test_the_baseline_covers_every_official_file_the_spec_names(self):
        self.assertEqual(set(BASELINE['files']), set(eb.OFFICIAL_FILES))
        self.assertEqual(set(BASELINE['results']), set(eb.SPLITS))

    def test_official_results_on_all_three_public_splits_are_unchanged(self):
        for split, digest in BASELINE['results'].items():
            self.assertEqual(eb.official_results_hash(split), digest, split)


class SchemaSeparationTests(unittest.TestCase):
    def test_the_official_result_schema_rejects_an_extension_result(self):
        import extension_rules as ex
        from ext_world import claim, line
        from schema_subset import validate
        official = json.loads((ROOT / 'schemas' / 'result.schema.json').read_text(encoding='utf-8'))
        for result in ex.evaluate_extensions(claim([line(1, 'SVC-LAB')]), None):
            with self.assertRaises(ValueError):
                validate(result, official)

    def test_no_extension_rule_id_looks_like_an_official_one(self):
        import extension_rules as ex
        for rid in ex.DETAIL_FUNCS:
            self.assertTrue(rid.startswith('E') and not rid.startswith('R'), rid)


if __name__ == '__main__':
    unittest.main()
