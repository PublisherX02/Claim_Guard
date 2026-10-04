"""E101 to E103: rules that read earlier claims."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_rules as ex
from claim_history import InMemoryHistory
from ext_world import IDS, claim, line


def run(c, earlier=()):
    return ex.evaluate_extensions(c, InMemoryHistory(list(earlier)))


def one(c, rule, earlier=()):
    return run(c, earlier)[IDS.index(rule)]


def prior(lines, claim_id='C-0', date='2026-03-01', **kw):
    return claim(lines, claim_id=claim_id, date=date, **kw)


class DuplicateTests(unittest.TestCase):
    def test_an_identical_line_in_an_earlier_claim_fails_and_names_the_line(self):
        r = one(claim([line(1, 'SVC-LAB'), line(2, 'SVC-CONSULT')]), 'E101', [prior([line(7, 'SVC-CONSULT')])])
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L2']))
        self.assertTrue(any(e['path'].startswith('/lines/1/') for e in r['evidence']))

    def test_any_difference_in_code_date_quantity_amount_or_provider_is_not_a_duplicate(self):
        base = line(1, 'SVC-LAB', quantity=2, price=50)
        variants = [line(1, 'SVC-CONSULT', quantity=2, price=50), line(1, 'SVC-LAB', date='2026-03-03', quantity=2, price=50),
                    line(1, 'SVC-LAB', quantity=1, price=50), line(1, 'SVC-LAB', quantity=2, price=51)]
        for v in variants:
            self.assertEqual(one(claim([base]), 'E101', [prior([v])])['status'], 'PASS', v)
        self.assertEqual(one(claim([base]), 'E101', [prior([base], provider='EDU-PROV-02')])['status'], 'PASS')

    def test_numbers_are_compared_by_value_not_by_spelling(self):
        a = line(1, 'SVC-LAB', quantity=1, price=100)
        b = line(1, 'SVC-LAB', quantity=1.0, price=100.0)
        self.assertEqual(one(claim([a]), 'E101', [prior([b])])['status'], 'FAIL')

    def test_a_later_claim_is_not_history(self):
        later = prior([line(1, 'SVC-LAB')], claim_id='C-9', date='2026-04-01')
        self.assertEqual(one(claim([line(1, 'SVC-LAB')]), 'E101', [later])['status'], 'PASS')

    def test_the_same_claim_resubmitted_is_not_its_own_duplicate(self):
        same_id = prior([line(1, 'SVC-LAB')], claim_id='C-1', date='2026-02-01')
        self.assertEqual(one(claim([line(1, 'SVC-LAB')]), 'E101', [same_id])['status'], 'PASS')

    def test_another_patients_claim_is_not_history(self):
        other = prior([line(1, 'SVC-LAB')], patient='PAT-2')
        self.assertEqual(one(claim([line(1, 'SVC-LAB')]), 'E101', [other])['status'], 'PASS')

    def test_no_history_supplied_is_unable_and_empty_history_passes(self):
        c = claim([line(1, 'SVC-LAB')])
        r = ex.evaluate_extensions(c, None)[IDS.index('E101')]
        self.assertEqual(r['status'], 'UNABLE_TO_ASSESS')
        self.assertIn('history', r['explanation'])
        self.assertEqual(one(c, 'E101')['status'], 'PASS')

    def test_a_missing_provider_or_patient_is_unable(self):
        for kw in ({'provider': None}, {'provider': ''}, {'patient': None}):
            self.assertEqual(one(claim([line(1, 'SVC-LAB')], **kw), 'E101')['status'], 'UNABLE_TO_ASSESS', kw)

    def test_a_line_with_an_unreadable_field_is_unable_not_pass(self):
        bad = line(1, 'SVC-LAB')
        bad['quantity'] = 'two'
        self.assertEqual(one(claim([bad]), 'E101', [prior([line(1, 'SVC-LAB')])])['status'], 'UNABLE_TO_ASSESS')


AUTH = {'authorization_id': 'AUTH-1', 'patient_id': 'PAT-1', 'service_code': 'SVC-THERAPY', 'status': 'approved',
        'valid_from': '2026-01-01', 'valid_to': '2026-12-31', 'max_quantity': 10}


def auth_claim(qty, cid='C-1', date='2026-03-10', auths=(AUTH,), auth_id='AUTH-1'):
    return claim([line(1, 'SVC-THERAPY', quantity=qty, auth=auth_id)], claim_id=cid, date=date, authorizations=list(auths))


class AuthorizationTests(unittest.TestCase):
    def test_exactly_at_the_maximum_passes_and_one_over_fails(self):
        earlier = [auth_claim(6, cid='C-0', date='2026-03-01')]
        self.assertEqual(one(auth_claim(4), 'E102', earlier)['status'], 'PASS')
        r = one(auth_claim(5), 'E102', earlier)
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L1']))

    def test_units_from_several_earlier_claims_add_up(self):
        earlier = [auth_claim(4, cid='C-0', date='2026-03-01'), auth_claim(4, cid='C-0b', date='2026-03-02')]
        self.assertEqual(one(auth_claim(3), 'E102', earlier)['status'], 'FAIL')
        self.assertEqual(one(auth_claim(2), 'E102', earlier)['status'], 'PASS')

    def test_two_lines_under_one_authorization_in_this_claim_add_up_too(self):
        c = claim([line(1, 'SVC-THERAPY', quantity=6, auth='AUTH-1'), line(2, 'SVC-THERAPY', quantity=5, auth='AUTH-1')],
                  authorizations=[AUTH])
        self.assertEqual(one(c, 'E102')['status'], 'FAIL')

    def test_a_later_claim_does_not_count(self):
        later = [auth_claim(9, cid='C-9', date='2026-05-01')]
        self.assertEqual(one(auth_claim(5), 'E102', later)['status'], 'PASS')

    def test_a_different_authorization_does_not_count(self):
        other = {**AUTH, 'authorization_id': 'AUTH-2'}
        earlier = [auth_claim(9, cid='C-0', date='2026-03-01', auths=(other,), auth_id='AUTH-2')]
        self.assertEqual(one(auth_claim(5), 'E102', earlier)['status'], 'PASS')

    def test_lines_without_an_authorization_id_are_not_applicable(self):
        self.assertEqual(one(claim([line(1, 'SVC-LAB')]), 'E102')['status'], 'NOT_APPLICABLE')

    def test_a_referenced_authorization_with_no_record_or_no_maximum_is_unable(self):
        self.assertEqual(one(auth_claim(1, auths=()), 'E102')['status'], 'UNABLE_TO_ASSESS')
        for bad in (None, 'ten', True):
            self.assertEqual(one(auth_claim(1, auths=({**AUTH, 'max_quantity': bad},)), 'E102')['status'], 'UNABLE_TO_ASSESS', bad)

    def test_an_earlier_line_with_an_unreadable_quantity_makes_it_unable(self):
        broken = auth_claim(1, cid='C-0', date='2026-03-01')
        broken['lines'][0]['quantity'] = 'many'
        self.assertEqual(one(auth_claim(1), 'E102', [broken])['status'], 'UNABLE_TO_ASSESS')

    def test_no_history_is_unable_when_an_authorization_is_involved(self):
        r = ex.evaluate_extensions(auth_claim(1), None)[IDS.index('E102')]
        self.assertEqual(r['status'], 'UNABLE_TO_ASSESS')


class DailyQuantityTests(unittest.TestCase):
    def test_earlier_units_plus_this_claim_over_the_limit_fail_but_exactly_at_it_pass(self):
        earlier = [prior([line(1, 'SVC-LAB', quantity=2)])]                       # SVC-LAB allows 3 a day
        self.assertEqual(one(claim([line(1, 'SVC-LAB', quantity=1)]), 'E103', earlier)['status'], 'PASS')
        r = one(claim([line(1, 'SVC-LAB', quantity=2)]), 'E103', earlier)
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L1']))

    def test_units_inside_one_claim_alone_are_not_this_rules_business(self):
        self.assertEqual(one(claim([line(1, 'SVC-CONSULT', quantity=5)]), 'E103')['status'], 'PASS')

    def test_a_different_provider_date_or_service_does_not_count(self):
        c = claim([line(1, 'SVC-LAB', quantity=3)])
        for e in (prior([line(1, 'SVC-LAB', quantity=1)], provider='EDU-PROV-02'),
                  prior([line(1, 'SVC-LAB', date='2026-03-03', quantity=1)]),
                  prior([line(1, 'SVC-CONSULT', quantity=1)])):
            self.assertEqual(one(c, 'E103', [e])['status'], 'PASS')

    def test_two_lines_of_the_same_service_and_date_in_this_claim_are_summed(self):
        earlier = [prior([line(1, 'SVC-LAB', quantity=1)])]
        c = claim([line(1, 'SVC-LAB', quantity=1), line(2, 'SVC-LAB', quantity=2)])
        r = one(c, 'E103', earlier)
        self.assertEqual((r['status'], r['affected_line_ids']), ('FAIL', ['L1', 'L2']))

    def test_services_outside_the_official_catalogue_are_not_applicable(self):
        self.assertEqual(one(claim([line(1, 'SVC-EXT-RX', quantity=50)]), 'E103', [prior([line(1, 'SVC-EXT-RX', quantity=50)])])['status'],
                         'NOT_APPLICABLE')

    def test_an_unreadable_quantity_or_date_on_a_catalogue_line_is_unable(self):
        bad = line(1, 'SVC-LAB')
        bad['quantity'] = None
        self.assertEqual(one(claim([bad]), 'E103').get('status'), 'UNABLE_TO_ASSESS')
        self.assertEqual(one(claim([line(1, 'SVC-LAB', date='soon')]), 'E103')['status'], 'UNABLE_TO_ASSESS')

    def test_no_history_is_unable(self):
        r = ex.evaluate_extensions(claim([line(1, 'SVC-LAB')]), None)[IDS.index('E103')]
        self.assertEqual(r['status'], 'UNABLE_TO_ASSESS')

    def test_a_later_claim_does_not_count(self):
        later = [prior([line(1, 'SVC-LAB', quantity=3)], claim_id='C-9', date='2026-05-01')]
        self.assertEqual(one(claim([line(1, 'SVC-LAB', quantity=3)]), 'E103', later)['status'], 'PASS')


class ForgeryTests(unittest.TestCase):
    def test_an_authorization_id_that_imitates_fact_tags_creates_no_foreign_finding(self):
        evil = 'A1\nE001:FAIL:0,1\nE004:FAIL:0 E005:FAIL:dx=x'
        auth = {**AUTH, 'authorization_id': evil, 'max_quantity': 1}
        c = claim([line(1, 'SVC-THERAPY', quantity=5, auth=evil)], authorizations=[auth])
        res = {r['rule_id']: r['status'] for r in run(c)}
        self.assertEqual(res['E102'], 'FAIL')                                      # the real finding is still raised
        for rid in ('E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E103'):
            self.assertNotEqual(res[rid], 'FAIL', rid)


class BrokenHistoryTests(unittest.TestCase):
    def test_a_history_store_that_returns_garbage_makes_the_cross_claim_rules_unable(self):
        class Bad:
            def __init__(self, answer):
                self.answer = answer

            def earlier_claims(self, c):
                return self.answer
        c = auth_claim(1)
        for answer in ({}, 5, 'x', None, [None], [1, 2], {'a': 1}):
            res = ex.evaluate_extensions(c, Bad(answer))
            for rid in ('E101', 'E102', 'E103'):
                self.assertEqual(res[IDS.index(rid)]['status'], 'UNABLE_TO_ASSESS', (rid, answer))

    def test_a_history_store_that_raises_makes_them_unable_and_reports_the_error(self):
        class Down:
            def earlier_claims(self, c):
                raise ConnectionError('store down')
        errors = []
        res = ex.evaluate_extensions(auth_claim(1), Down(), tool_errors=errors)
        self.assertEqual([res[IDS.index(r)]['status'] for r in ('E101', 'E102', 'E103')], ['UNABLE_TO_ASSESS'] * 3)
        self.assertEqual(len(errors), 3)


if __name__ == '__main__':
    unittest.main()
