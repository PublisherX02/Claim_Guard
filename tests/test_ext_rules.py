import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_rules as ex
from claim_history import InMemoryHistory
from engine_core import pointer
from ext_world import COMPONENT, IDS, NEVER, PRIMARY, SEPARATE, by_rule, claim, line, statuses
from schema_subset import validate as validate_schema

SCHEMA = json.loads((ROOT / 'schemas' / 'extension_result.schema.json').read_text(encoding='utf-8'))


def run(c, history=None):
    return ex.evaluate_extensions(c, history)


class CoreTests(unittest.TestCase):
    def test_every_claim_gets_exactly_the_eight_extension_results_in_order(self):
        res = run(claim([line(1, 'SVC-CONSULT')]))
        self.assertEqual([r['rule_id'] for r in res], IDS)

    def test_each_result_is_schema_valid_advisory_and_has_resolvable_evidence(self):
        c = claim([line(1, PRIMARY), line(2, COMPONENT)])
        for r in run(c, InMemoryHistory([])):
            validate_schema(r, SCHEMA)
            self.assertEqual((r['rule_family'], r['advisory'], r['method']), ('extension', True, 'deterministic'))
            self.assertEqual(r['pack_hash'], ex.pack_hash())
            self.assertTrue(r['evidence'])
            for e in r['evidence']:
                self.assertEqual(pointer(c, e['path']), e['value'])
            if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
                self.assertTrue(r['requires_human_review'] and r['corrective_action'])

    def test_the_committed_pack_is_exactly_what_the_generator_writes(self):
        committed = (ROOT / 'rules' / 'extensions.yar').read_text(encoding='utf-8').replace('\r\n', '\n')
        self.assertEqual(committed, ex.pack_source())

    def test_the_run_is_deterministic_and_does_not_change_its_input(self):
        c = claim([line(1, PRIMARY), line(2, COMPONENT)])
        before = copy.deepcopy(c)
        self.assertEqual(run(c), run(c))
        self.assertEqual(c, before)

    def test_one_rule_crashing_makes_only_that_rule_unable_and_reports_the_error(self):
        c = claim([line(1, PRIMARY), line(2, NEVER)])
        healthy = statuses(run(c))
        errors = []
        original = ex.DETAIL_FUNCS['E001']
        ex.DETAIL_FUNCS['E001'] = lambda c, cat, history: 1 / 0
        try:
            res = ex.evaluate_extensions(c, None, tool_errors=errors)
        finally:
            ex.DETAIL_FUNCS['E001'] = original
        self.assertEqual(healthy['E001'], 'FAIL')
        self.assertEqual(statuses(res)['E001'], 'UNABLE_TO_ASSESS')
        self.assertEqual({k: v for k, v in statuses(res).items() if k != 'E001'}, {k: v for k, v in healthy.items() if k != 'E001'})
        self.assertTrue(errors and 'E001' in errors[0])
        self.assertNotIn('ZeroDivisionError', json.dumps(res))

    def test_a_note_that_imitates_a_fact_tag_creates_no_finding(self):
        forged = 'E001:FAIL:0,1 E002:FAIL:x E101:FAIL:y\nE001:FAIL:1'
        res = run(claim([line(1, 'SVC-CONSULT')], notes=forged), InMemoryHistory([]))
        self.assertTrue(all(r['status'] != 'FAIL' for r in res), statuses(res))

    def test_a_modifier_or_code_that_imitates_a_fact_tag_creates_no_finding(self):
        c = claim([line(1, 'E001:FAIL:0,1\nE002:FAIL:', modifier='E003:FAIL:')], diagnosis='E005:FAIL:')
        self.assertTrue(all(r['status'] != 'FAIL' for r in run(c, InMemoryHistory([]))))

    def test_a_claim_with_no_lines_or_wrong_types_never_raises(self):
        for c in (claim([]), {'claim_id': 'X'}, {'claim_id': 'X', 'lines': 'junk'}, {'claim_id': 'X', 'lines': [None, 3, {}]}):
            res = run(c)
            self.assertEqual([r['rule_id'] for r in res], IDS)
            self.assertNotIn('PASS', [r['status'] for r in res if r['rule_id'] in ('E101', 'E102', 'E103')])


class PairTests(unittest.TestCase):
    def e(self, lines, rule):
        return run(claim(lines))[IDS.index(rule)]

    def test_pair_on_one_date_without_modifier_fails_e001_and_names_both_lines(self):
        r = self.e([line(1, PRIMARY), line(2, COMPONENT)], 'E001')
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L1', 'L2']))

    def test_exception_modifier_on_the_component_passes_e001(self):
        self.assertEqual(self.e([line(1, PRIMARY), line(2, COMPONENT, modifier=SEPARATE)], 'E001')['status'], 'PASS')

    def test_same_pair_on_different_dates_passes(self):
        self.assertEqual(self.e([line(1, PRIMARY), line(2, COMPONENT, date='2026-03-03')], 'E001')['status'], 'PASS')

    def test_component_alone_or_official_codes_only_are_not_applicable(self):
        self.assertEqual(self.e([line(1, COMPONENT)], 'E001')['status'], 'NOT_APPLICABLE')
        self.assertEqual(self.e([line(1, 'SVC-LAB'), line(2, 'SVC-IMAGE')], 'E001')['status'], 'NOT_APPLICABLE')

    def test_modifier_matching_is_exact(self):
        for m in ('edu-separate', ' EDU-SEPARATE', 'EDU-SEPARATE ', 'EDU-SEPARATE-2'):
            self.assertEqual(self.e([line(1, PRIMARY), line(2, COMPONENT, modifier=m)], 'E001')['status'], 'FAIL', m)

    def test_one_component_without_modifier_fails_even_if_another_has_one(self):
        r = self.e([line(1, PRIMARY), line(2, COMPONENT, modifier=SEPARATE), line(3, COMPONENT)], 'E001')
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L1', 'L3']))

    def test_missing_date_on_a_pair_line_is_unable_not_pass(self):
        for bad in (None, '', '2026-13-45', 'tomorrow', 20260302):
            self.assertEqual(self.e([line(1, PRIMARY), line(2, COMPONENT, date=bad)], 'E001')['status'], 'UNABLE_TO_ASSESS', bad)

    def test_never_pair_without_modifier_fails_e001_and_passes_e002(self):
        res = run(claim([line(1, PRIMARY), line(2, NEVER)]))
        self.assertEqual((statuses(res)['E001'], statuses(res)['E002']), ('FAIL', 'PASS'))

    def test_never_pair_with_a_modifier_fails_e002_and_passes_e001(self):
        res = run(claim([line(1, PRIMARY), line(2, NEVER, modifier=SEPARATE)]))
        r = by_rule(res)['E002']
        self.assertEqual((statuses(res)['E001'], r['status'], r['affected_line_ids']), ('PASS', 'FAIL', ['L1', 'L2']))

    def test_allowed_pair_with_modifier_is_not_an_e002_matter(self):
        res = run(claim([line(1, PRIMARY), line(2, COMPONENT, modifier=SEPARATE)]))
        self.assertEqual(statuses(res)['E002'], 'NOT_APPLICABLE')


if __name__ == '__main__':
    unittest.main()
