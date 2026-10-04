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


ACC, SEC = 'DX-EXT-ACCIDENT', 'DX-EXT-SECONDARY'
RX_CODE = 'SVC-EXT-RX'
ATT = {'attachment_id': 'D1', 'type': 'service-note', 'patient_id': 'PAT-1', 'service_code': 'SVC-CONSULT', 'service_date': '2026-03-02',
       'document_status': 'final'}


def attachment(text):
    return {**ATT, 'text': text}


class EventDateTests(unittest.TestCase):
    def e3(self, notes='Synthetic claim.', attachments=None, diagnosis=ACC, lines=None):
        c = claim(lines or [line(1, 'SVC-CONSULT')], diagnosis=diagnosis, notes=notes, attachments=attachments)
        return run(c)[IDS.index('E003')]

    def test_an_event_date_before_the_first_service_passes(self):
        self.assertEqual(self.e3('Accident. Event date: 2026-03-01.')['status'], 'PASS')

    def test_an_event_date_equal_to_the_first_service_date_passes(self):
        self.assertEqual(self.e3('Event date: 2026-03-02')['status'], 'PASS')

    def test_an_event_date_after_the_first_service_fails(self):
        self.assertEqual(self.e3('Event date: 2026-03-03')['status'], 'FAIL')

    def test_no_event_date_fails(self):
        for notes in ('Synthetic claim.', '', None):
            self.assertEqual(self.e3(notes)['status'], 'FAIL', notes)

    def test_the_date_may_be_in_an_attachment(self):
        r = self.e3(attachments=[attachment('Record. Event date: 2026-02-27')])
        self.assertEqual(r['status'], 'PASS')
        self.assertIn('/attachments/0/text', [e['path'] for e in r['evidence']])

    def test_an_impossible_calendar_date_does_not_count(self):
        for text in ('Event date: 2026-02-30', 'Event date: 2026-13-01', 'Event date: 26-03-01', 'Event date: 2026-03-011'):
            self.assertEqual(self.e3(text)['status'], 'FAIL', text)

    def test_the_label_is_exact(self):
        for text in ('event date: 2026-03-01', 'EVENT DATE: 2026-03-01', 'Date of event: 2026-03-01', 'Event  date: 2026-03-01'):
            self.assertEqual(self.e3(text)['status'], 'FAIL', text)

    def test_one_good_date_among_bad_ones_passes(self):
        self.assertEqual(self.e3('Event date: 2026-02-30 and later Event date: 2026-03-01')['status'], 'PASS')

    def test_a_diagnosis_without_the_requirement_is_not_applicable(self):
        for dx in ('DX-EDU-01', SEC, 'anything'):
            self.assertEqual(self.e3(diagnosis=dx)['status'], 'NOT_APPLICABLE', dx)

    def test_a_missing_diagnosis_is_unable(self):
        for dx in (None, '', 5):
            self.assertEqual(self.e3(diagnosis=dx)['status'], 'UNABLE_TO_ASSESS', dx)

    def test_without_any_readable_service_date_it_is_unable_not_pass(self):
        self.assertEqual(self.e3('Event date: 2026-03-01', lines=[line(1, 'SVC-CONSULT', date='someday')])['status'], 'UNABLE_TO_ASSESS')

    def test_the_earliest_service_date_is_the_one_compared(self):
        lines = [line(1, 'SVC-CONSULT', date='2026-03-09'), line(2, 'SVC-LAB', date='2026-03-02')]
        self.assertEqual(self.e3('Event date: 2026-03-05', lines=lines)['status'], 'FAIL')


class RouteTests(unittest.TestCase):
    def e4(self, lines):
        return run(claim(lines))[IDS.index('E004')]

    def test_a_pharmaceutical_line_with_a_route_modifier_passes(self):
        for m in ('EDU-ROUTE-ORAL', 'EDU-ROUTE-IV', 'EDU-ROUTE-TOPICAL'):
            self.assertEqual(self.e4([line(1, RX_CODE, modifier=m)])['status'], 'PASS', m)

    def test_missing_or_wrong_modifier_fails_and_names_only_the_bad_line(self):
        for m in (None, '', SEPARATE, 'edu-route-oral', 'EDU-ROUTE-ORAL '):
            r = self.e4([line(1, RX_CODE, modifier='EDU-ROUTE-IV'), line(2, RX_CODE, modifier=m)])
            self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L2']), m)

    def test_other_services_are_not_applicable_including_the_official_pharmacy_code(self):
        self.assertEqual(self.e4([line(1, 'SVC-PHARM'), line(2, 'SVC-CONSULT')])['status'], 'NOT_APPLICABLE')
        self.assertEqual(self.e4([])['status'], 'NOT_APPLICABLE')


class PrimaryDiagnosisTests(unittest.TestCase):
    def e5(self, dx):
        return run(claim([line(1, 'SVC-CONSULT')], diagnosis=dx))[IDS.index('E005')]

    def test_a_secondary_only_diagnosis_cannot_be_the_header_diagnosis(self):
        r = self.e5(SEC)
        self.assertEqual(r['status'], 'FAIL')
        self.assertIn('/diagnosis_code', [e['path'] for e in r['evidence']])

    def test_ordinary_and_unlisted_diagnoses_pass(self):
        for dx in ('DX-EDU-01', ACC, 'unknown-code'):
            self.assertEqual(self.e5(dx)['status'], 'PASS', dx)

    def test_missing_diagnosis_is_unable(self):
        for dx in (None, '', 7):
            self.assertEqual(self.e5(dx)['status'], 'UNABLE_TO_ASSESS', dx)


if __name__ == '__main__':
    unittest.main()
