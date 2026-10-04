"""Fuzz the extension rules: whatever arrives, eight valid advisory results come back, nothing raises, and hostile text forges nothing."""
import json
import logging
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_rules as ex
import fuzz_strategies as fs
from claim_history import InMemoryHistory
from engine_core import pointer
from ext_world import IDS, claim, line
from hypothesis import given, strategies as st
from schema_subset import validate as validate_schema

SCHEMA = json.loads((ROOT / 'schemas' / 'extension_result.schema.json').read_text(encoding='utf-8'))
ALLOWED = {'PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE'}
CLAIM_KEYS = ['claim_id', 'patient_id', 'provider_id', 'submission_date', 'diagnosis_code', 'notes', 'lines', 'attachments', 'authorizations']
LINE_KEYS = ['line_id', 'service_code', 'service_date', 'modifier', 'quantity', 'unit_price', 'net_amount', 'authorization_id']


def setUpModule():
    logging.disable(logging.WARNING)


def tearDownModule():
    logging.disable(logging.NOTSET)


def check(test, c, history=None):
    res = ex.evaluate_extensions(c, history)
    test.assertEqual([r['rule_id'] for r in res], IDS)
    for r in res:
        test.assertIn(r['status'], ALLOWED)
        validate_schema(r, SCHEMA)
        if isinstance(c, dict):
            for e in r['evidence']:
                try:
                    test.assertEqual(pointer(c, e['path']), e['value'])
                except (KeyError, IndexError, TypeError, ValueError):
                    test.assertEqual(e['path'], '/claim_id')          # the documented fallback when nothing resolves
    return res


junk_lines = st.lists(st.one_of(st.dictionaries(st.sampled_from(LINE_KEYS), fs.json_values, max_size=8), fs.json_values), max_size=6)
junk_claims = st.dictionaries(st.sampled_from(CLAIM_KEYS), st.one_of(fs.json_values, junk_lines), max_size=9)


class ExtensionFuzz(unittest.TestCase):
    @given(junk_claims)
    def test_any_claim_shaped_json_gets_eight_valid_results(self, c):
        check(self, c, None)

    @given(junk_claims, st.lists(junk_claims, max_size=4))
    def test_any_claim_with_any_history_gets_eight_valid_results(self, c, earlier):
        check(self, c, InMemoryHistory(earlier + [c]))

    @given(st.lists(fs.json_values, max_size=6))
    def test_a_history_built_from_arbitrary_json_never_raises(self, items):
        h = InMemoryHistory(items)
        self.assertIsInstance(h.earlier_claims(claim([line(1, 'SVC-LAB')])), list)

    @given(fs.json_values)
    def test_a_history_object_that_misbehaves_makes_the_history_rules_unable_not_a_crash(self, junk):
        class Broken:
            def earlier_claims(self, c):
                return junk
        res = ex.evaluate_extensions(claim([line(1, 'SVC-LAB', auth='A1')]), Broken())
        self.assertEqual([r['rule_id'] for r in res], IDS)
        readable = isinstance(junk, (list, tuple)) and all(isinstance(x, dict) for x in junk)
        if not readable:                    # a store that returns garbage must fail closed, never read as 'no earlier claims'
            for r in res:
                if r['rule_id'] in ('E101', 'E102', 'E103'):
                    self.assertEqual(r['status'], 'UNABLE_TO_ASSESS', (r['rule_id'], junk))

    @given(fs.hostile_text, fs.hostile_text, fs.hostile_text)
    def test_hostile_text_in_any_text_field_changes_no_verdict(self, note, code, modifier):
        base = claim([line(1, 'SVC-EXT-PRIMARY'), line(2, 'SVC-EXT-COMPONENT', modifier='EDU-SEPARATE'), line(3, 'SVC-LAB')],
                     diagnosis='DX-EXT-ACCIDENT')
        want = {r['rule_id']: r['status'] for r in ex.evaluate_extensions(base, InMemoryHistory([]))}
        hostile = claim([line(1, 'SVC-EXT-PRIMARY'), line(2, 'SVC-EXT-COMPONENT', modifier='EDU-SEPARATE'), line(3, 'SVC-LAB')],
                        diagnosis='DX-EXT-ACCIDENT', notes=note)
        hostile['lines'][2]['modifier'] = modifier
        hostile['patient_id'] = code
        got = {r['rule_id']: r['status'] for r in ex.evaluate_extensions(hostile, InMemoryHistory([]))}
        for rid in ('E001', 'E002', 'E003', 'E004', 'E005'):
            self.assertEqual(got[rid], want[rid], rid)

    @given(st.integers(0, 40))
    def test_a_large_claim_and_history_are_handled(self, n):
        c = claim([line(i + 1, 'SVC-LAB', quantity=1) for i in range(n * 12)])
        earlier = [claim([line(1, 'SVC-LAB')], claim_id=f'H{i}', date='2026-03-01') for i in range(n * 5)]
        check(self, c, InMemoryHistory(earlier))


if __name__ == '__main__':
    unittest.main()
