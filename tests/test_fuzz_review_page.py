"""Whatever text a claim carries, the review page must show it as data, never run it."""
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
from hypothesis import given, strategies as st
from make_review import build


def row(text):
    return {'claim_id': 'CG-1', 'rule_id': 'R001', 'rule_version': '1.0.0', 'status': 'FAIL', 'severity': 'high',
            'affected_line_ids': [], 'evidence': [{'path': '/notes', 'value': text}], 'rule_source': text,
            'explanation': text, 'corrective_action': text, 'confidence': None, 'confidence_kind': 'not_probabilistic',
            'requires_human_review': True, 'method': 'deterministic', 'review_status': 'unreviewed'}


def data_blob(page):
    start = page.index('const rows=') + len('const rows=')
    return page[start:page.index(', decisions=[]')]


class ReviewPageFuzz(unittest.TestCase):
    @given(st.lists(fs.hostile_text, min_size=1, max_size=5))
    def test_one_script_block_and_data_round_trips(self, texts):
        rows = [row(t) for t in texts]
        page = build(rows)
        self.assertEqual(page.count('<script'), 1)
        self.assertEqual(page.count('</script>'), 1)
        self.assertEqual(json.loads(data_blob(page)), rows)

    @given(fs.json_values)
    def test_arbitrary_evidence_values_never_break_the_page(self, value):
        r = row('x')
        r['evidence'] = [{'path': '/a', 'value': value}]
        page = build([r])
        self.assertEqual(page.count('</script>'), 1)
        self.assertEqual(json.loads(data_blob(page)), [r])

    @given(st.lists(fs.hostile_text, min_size=1, max_size=3))
    def test_the_data_blob_contains_no_raw_markup_characters(self, texts):
        blob = data_blob(build([row(t) for t in texts]))
        for ch in '<>&':
            self.assertNotIn(ch, blob)

    @given(st.lists(fs.hostile_text, min_size=1, max_size=3))
    def test_the_data_blob_has_no_raw_js_line_terminators(self, texts):
        # U+2028/U+2029 were illegal inside a JavaScript string literal before ES2019
        blob = data_blob(build([row(t) for t in texts]))
        for ch in (' ', ' ', '\n', '\r'):
            self.assertNotIn(ch, blob)


if __name__ == '__main__':
    unittest.main()
