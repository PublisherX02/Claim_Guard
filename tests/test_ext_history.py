import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import claim_history as ch


def claim(cid, patient='PAT-1', date='2026-03-01'):
    return {'claim_id': cid, 'patient_id': patient, 'submission_date': date, 'lines': []}


class HistoryTests(unittest.TestCase):
    def ids(self, history, c):
        return [x['claim_id'] for x in history.earlier_claims(c)]

    def test_only_strictly_earlier_claims_of_the_same_patient_count(self):
        h = ch.InMemoryHistory([claim('C1', date='2026-02-01'), claim('C2', date='2026-03-01'), claim('C3', date='2026-04-01'),
                                claim('C9', patient='PAT-2', date='2026-01-01')])
        self.assertEqual(self.ids(h, claim('C2', date='2026-03-01')), ['C1'])
        self.assertEqual(self.ids(h, claim('C3', date='2026-04-01')), ['C1', 'C2'])
        self.assertEqual(self.ids(h, claim('C1', date='2026-02-01')), [])

    def test_equal_submission_dates_are_ordered_by_claim_id_so_exactly_one_is_earlier(self):
        h = ch.InMemoryHistory([claim('A'), claim('B')])
        self.assertEqual(self.ids(h, claim('B')), ['A'])
        self.assertEqual(self.ids(h, claim('A')), [])

    def test_a_claim_is_never_its_own_history_even_when_resubmitted_under_the_same_id(self):
        h = ch.InMemoryHistory([claim('A', date='2026-01-01')])
        self.assertEqual(self.ids(h, claim('A', date='2026-05-01')), [])

    def test_results_are_in_a_stable_order_whatever_the_input_order(self):
        cs = [claim('C3', date='2026-03-01'), claim('C1', date='2026-01-01'), claim('C2', date='2026-02-01')]
        a = ch.InMemoryHistory(cs).earlier_claims(claim('Z', date='2026-12-01'))
        b = ch.InMemoryHistory(list(reversed(cs))).earlier_claims(claim('Z', date='2026-12-01'))
        self.assertEqual([x['claim_id'] for x in a], ['C1', 'C2', 'C3'])
        self.assertEqual(a, b)

    def test_a_claim_without_a_usable_patient_id_has_no_history(self):
        h = ch.InMemoryHistory([claim('A', patient=None), claim('B', patient='')])
        for pid in (None, '', 7, [], {}):
            c = claim('Z', patient=pid, date='2027-01-01')
            self.assertEqual(h.earlier_claims(c), [], pid)

    def test_a_missing_or_malformed_submission_date_never_raises(self):
        h = ch.InMemoryHistory([claim('A'), {'claim_id': 'B', 'patient_id': 'PAT-1'}, 'junk', None, {'patient_id': 'PAT-1'}])
        for date in (None, '', 5, 'not-a-date'):
            c = claim('Z', date=date)
            self.assertIsInstance(h.earlier_claims(c), list)

    def test_the_stored_claims_cannot_be_changed_through_what_is_returned(self):
        original = claim('A', date='2026-01-01')
        original['lines'] = [{'quantity': 1}]
        h = ch.InMemoryHistory([original])
        h.earlier_claims(claim('Z', date='2026-06-01'))[0]['lines'][0]['quantity'] = 99
        self.assertEqual(h.earlier_claims(claim('Z', date='2026-06-01'))[0]['lines'][0]['quantity'], 1)
        self.assertEqual(original['lines'][0]['quantity'], 1)

    def test_building_the_history_does_not_change_its_input(self):
        cs = [claim('A', date='2026-01-01')]
        before = copy.deepcopy(cs)
        ch.InMemoryHistory(cs)
        self.assertEqual(cs, before)


class CatalogueTests(unittest.TestCase):
    cat = json.loads((ROOT / 'rules' / 'extensions' / 'catalogue.json').read_text(encoding='utf-8'))

    def test_the_catalogue_says_it_is_fictional(self):
        self.assertIn('FICTIONAL', self.cat['_note'].upper())

    def test_every_invented_code_is_outside_the_official_teaching_catalogue(self):
        services = json.loads((ROOT / 'rules' / 'services.json').read_text(encoding='utf-8'))
        diagnoses = {d['code'] for d in json.loads((ROOT / 'rules' / 'diagnoses.json').read_text(encoding='utf-8'))}
        invented_services = set(self.cat['services'])
        for pair in self.cat['pairs']:
            invented_services |= {pair['primary'], pair['secondary']}
        self.assertTrue(invented_services)
        self.assertFalse(invented_services & set(services))
        self.assertTrue(set(self.cat['diagnoses']))
        self.assertFalse(set(self.cat['diagnoses']) & diagnoses)

    def test_eight_rules_are_declared_with_extension_ids_only(self):
        ids = [r['rule_id'] for r in self.cat['rules']]
        self.assertEqual(ids, ['E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103'])
        for r in self.cat['rules']:
            self.assertEqual(r['version'], '1.0.0')
            self.assertEqual(r['source'], f"fictional-extension/{r['rule_id']}@1.0.0")
            self.assertIn(r['severity'], ('high', 'medium', 'low'))
            self.assertTrue(r['corrective_action'].strip() and r['name'].strip())


if __name__ == '__main__':
    unittest.main()
