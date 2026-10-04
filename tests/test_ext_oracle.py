"""The engine and the independent oracle must agree on every generated claim and history."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_rules as ex
import oracle_extensions as oracle
from claim_history import InMemoryHistory
from ext_world import CLEAN
from hypothesis import HealthCheck, given, settings, strategies as st

DATES = st.sampled_from(['2026-03-02'] * 6 + ['2026-03-03'] * 3 + ['2026-03-01', '2026-02-30', None, '', 'soon'])
CODES = st.sampled_from(['SVC-EXT-PRIMARY'] * 4 + ['SVC-EXT-COMPONENT'] * 4 + ['SVC-EXT-NEVER'] * 2 + ['SVC-EXT-RX'] * 2 +
                        ['SVC-LAB'] * 3 + ['SVC-CONSULT', 'SVC-THERAPY', None])
MODS = st.sampled_from([None, None, '', 'EDU-SEPARATE', 'EDU-SEPARATE', 'EDU-ROUTE-IV', 'EDU-ROUTE-ORAL', 'x'])
QTY = st.one_of(*([st.integers(0, 4)] * 6), st.sampled_from([0.5, 1.5, 2.0, -1, -3]), st.just(None))
AUTH_IDS = st.sampled_from([None, None, 'A1', 'A1', 'A1', 'A2', ''])
NOTES = st.sampled_from([None, '', 'x', 'Event date: 2026-03-01', 'Event date: 2026-03-05', 'Event date: 2026-02-30', 'Event date:2026-03-02'])
DIAGNOSES = st.sampled_from(['DX-EXT-ACCIDENT', 'DX-EXT-ACCIDENT', 'DX-EXT-SECONDARY', 'DX-EDU-01', None, ''])
SUBMITTED = st.sampled_from(['2026-03-10', '2026-03-11', '2026-04-01', None, 'soon'])
LIMITS = st.one_of(*([st.integers(0, 12)] * 8), st.just(None), st.just('x'))


@st.composite
def lines(draw):
    n = draw(st.integers(0, 4))
    out = []
    for i in range(n):
        q, price = draw(QTY), draw(st.one_of(st.integers(1, 3), st.just(None)))
        net = draw(st.one_of(st.just(None if q is None or price is None else q * price), st.integers(0, 9)))
        out.append({'line_id': f'L{i + 1}', 'service_code': draw(CODES), 'service_date': draw(DATES), 'modifier': draw(MODS),
                    'quantity': q, 'unit_price': price, 'net_amount': net, 'authorization_id': draw(AUTH_IDS)})
    return out


@st.composite
def claims(draw, claim_id):
    c = dict(CLEAN)
    c.update(claim_id=claim_id, patient_id=draw(st.sampled_from(['P1', 'P1', 'P1', 'P2', None])),
             provider_id=draw(st.sampled_from(['V1', 'V1', 'V2', None])), submission_date=draw(SUBMITTED),
             diagnosis_code=draw(DIAGNOSES), notes=draw(NOTES), lines=draw(lines()))
    c['attachments'] = [{'attachment_id': 'D1', 'text': draw(NOTES)} for _ in range(draw(st.integers(0, 2)))]
    c['authorizations'] = [{'authorization_id': a, 'max_quantity': draw(LIMITS)}
                           for a in draw(st.sampled_from([['A1', 'A2'], ['A1', 'A2'], ['A1'], [], ['A2', 'A1', 'A1']]))]
    return c


@st.composite
def worlds(draw):
    ids = draw(st.lists(st.sampled_from(['C2', 'C3', 'C4']), min_size=0, max_size=3, unique=True))
    cs = [draw(claims('C1'))] + [draw(claims(i)) for i in ids]
    cs[0]['submission_date'] = draw(st.sampled_from(['2026-04-01'] * 8 + ['2026-03-11', None, '2026-02-30']))
    if draw(st.booleans()):                           # a pair scenario: primary plus component or never-bundle partner
        n = len(cs[0]['lines'])
        day = draw(st.sampled_from(['2026-03-02', '2026-03-03']))
        partner = draw(st.sampled_from(['SVC-EXT-COMPONENT', 'SVC-EXT-NEVER']))
        cs[0]['lines'] += [
            {'line_id': f'L{n + 1}', 'service_code': 'SVC-EXT-PRIMARY', 'service_date': day, 'modifier': None, 'quantity': 1,
             'unit_price': 1, 'net_amount': 1, 'authorization_id': None},
            {'line_id': f'L{n + 2}', 'service_code': partner, 'service_date': draw(st.sampled_from([day, day, '2026-03-01'])),
             'modifier': draw(st.sampled_from([None, 'EDU-SEPARATE'])), 'quantity': 1, 'unit_price': 1, 'net_amount': 1,
             'authorization_id': None},
            {'line_id': f'L{n + 3}', 'service_code': 'SVC-EXT-RX', 'service_date': day,
             'modifier': draw(st.sampled_from(['EDU-ROUTE-IV', 'EDU-ROUTE-ORAL', None, 'x'])), 'quantity': 1, 'unit_price': 1,
             'net_amount': 1, 'authorization_id': None}]
    if len(cs) > 1 and draw(st.booleans()):           # an authorization used up across two claims of one patient and provider
        cs[0]['patient_id'], cs[0]['provider_id'], cs[0]['submission_date'] = 'P1', 'V1', '2026-04-01'
        cs[1].update(patient_id='P1', provider_id='V1', submission_date='2026-03-10')
        for c, qty in ((cs[0], draw(st.integers(1, 6))), (cs[1], draw(st.integers(1, 6)))):
            c['lines'] = [{'line_id': 'L1', 'service_code': 'SVC-THERAPY', 'service_date': '2026-03-02', 'modifier': None,
                           'quantity': qty, 'unit_price': 1, 'net_amount': qty, 'authorization_id': 'A1'}]
            c['authorizations'] = [{'authorization_id': 'A1', 'max_quantity': draw(st.integers(3, 9))}]
        return cs
    for other in cs[1:]:
        other['submission_date'] = draw(st.sampled_from(['2026-03-10', '2026-03-10', '2026-03-11', '2026-04-01', None]))
        if draw(st.booleans()):                       # an earlier claim that repeats the target's lines, patient and provider
            other.update(patient_id=cs[0]['patient_id'], provider_id=cs[0]['provider_id'])
            other['lines'] = [dict(l) for l in cs[0]['lines']]
            other['authorizations'] = [dict(a) for a in cs[0]['authorizations']]
    return cs


SEEN = {}
# statuses a rule can never give by its definition: E005 has no not-applicable case, and E001/E002/E004 cannot be unable except through dates
IMPOSSIBLE = {('E005', 'NOT_APPLICABLE'), ('E004', 'UNABLE_TO_ASSESS'), ('E101', 'NOT_APPLICABLE')}


def compare(test, everything):
    target = everything[0]
    got = {r['rule_id']: r for r in ex.evaluate_extensions(target, InMemoryHistory(everything))}
    want = oracle.oracle(target, everything)
    for rid, (status, affected) in want.items():
        test.assertEqual(got[rid]['status'], status, (rid, target, everything))
        SEEN[(rid, status)] = SEEN.get((rid, status), 0) + 1
        test.assertEqual(set(got[rid]['affected_line_ids']), affected, (rid, target, everything))
        if status == 'FAIL':
            test.assertEqual(len(got[rid]['affected_line_ids']), len(set(got[rid]['affected_line_ids'])))


class OracleAgreementTests(unittest.TestCase):
    @classmethod
    def tearDownClass(cls):
        # the agreement above would mean little if the generator never produced a status; every status of every rule must occur
        missing = [(r, st_) for r in ('E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103')
                   for st_ in ('PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE') if (r, st_) not in SEEN]
        if SEEN:
            assert not [m for m in missing if m not in IMPOSSIBLE], missing

    @settings(max_examples=500, deadline=None, suppress_health_check=[HealthCheck.too_slow, HealthCheck.filter_too_much])
    @given(worlds())
    def test_the_engine_and_the_oracle_agree_on_status_and_affected_lines(self, everything):
        compare(self, everything)

    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    @given(worlds())
    def test_without_history_no_cross_claim_rule_can_pass_or_fail(self, everything):
        got = {r['rule_id']: r['status'] for r in ex.evaluate_extensions(everything[0], None)}
        for rid in ('E101', 'E102', 'E103'):
            self.assertIn(got[rid], ('UNABLE_TO_ASSESS', 'NOT_APPLICABLE'), rid)

    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    @given(worlds())
    def test_a_claim_s_result_does_not_depend_on_later_claims(self, everything):
        target = everything[0]
        key = (target.get('submission_date') or '', target['claim_id'])
        not_later = [c for c in everything if (c.get('submission_date') or '', c['claim_id']) <= key or c is target]
        a = [(r['rule_id'], r['status']) for r in ex.evaluate_extensions(target, InMemoryHistory(everything))]
        b = [(r['rule_id'], r['status']) for r in ex.evaluate_extensions(target, InMemoryHistory(not_later))]
        self.assertEqual(a, b)


if __name__ == '__main__':
    unittest.main()
