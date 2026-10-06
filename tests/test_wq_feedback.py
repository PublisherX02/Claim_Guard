"""The feedback report: counts of what people finally decided per rule, exact intervals, no text, no identifiers, and no side effects."""
import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from workqueue import feedback
from wq_world import queue_doc, results_of, store_makers

SECRET_REASON = 'patient Maha Al-Test phoned me, member M-P1'


def decided(store, claim_id, flags, actions, signoff=None, escalated=False, extra=(), reason='r'):
    """Store a claim with these flagged findings and walk it to decided with {rule: final action}; `extra` is a list of
    (rule, action) recorded before the final ones."""
    doc = queue_doc(claim_id)
    doc['results'] = results_of(flags, claim_id)
    assert store.put_triaged(doc)
    assert store.transition(claim_id, 1, 'triaged', 'ready', 'system:t', 1000.0)
    assert store.lease(claim_id, 1, 'CG-2002', 1001.0, 5000.0)
    for rid, action in list(extra) + list(actions.items()):
        assert store.add_decision(claim_id, 1, 'CG-2002', 1002.0, {'rule_id': rid, 'action': action, 'actor': 'CG-2002',
                                                                    'reason': reason, 'at': 1002.0})
    fields = {'decided_by': 'CG-2002'}
    if signoff:
        fields['signoff'] = signoff
    if escalated:
        fields['escalated'] = True
    assert store.transition(claim_id, 1, 'leased', 'decided', 'CG-2002', 1003.0, set_fields=fields)


class FeedbackBase:
    def setUp(self):
        self.store, self.cleanup = self.factory()

    def tearDown(self):
        self.cleanup()

    def test_an_empty_store_reports_zeros_and_no_rates(self):
        rep = feedback.report(self.store)
        self.assertEqual(rep['claims_decided'], 0)
        self.assertEqual(rep['rules'], {})
        self.assertIsNone(rep['signoff']['disagreement_rate'])

    def test_final_actions_are_counted_per_rule(self):
        decided(self.store, 'C1', {'R001': ('FAIL', 'medium')}, {'R001': 'confirm_issue'})
        decided(self.store, 'C2', {'R001': ('FAIL', 'medium')}, {'R001': 'dismiss_with_reason'})
        decided(self.store, 'C3', {'R001': ('FAIL', 'medium'), 'R002': ('UNABLE_TO_ASSESS', 'high')},
                {'R001': 'dismiss_with_reason', 'R002': 'confirm_issue'})
        decided(self.store, 'C4', None, {})
        rep = feedback.report(self.store)
        self.assertEqual((rep['claims_decided'], rep['verified_clear']), (4, 1))
        r1, r2 = rep['rules']['R001'], rep['rules']['R002']
        self.assertEqual((r1['flagged'], r1['confirmed'], r1['dismissed'], r1['decisions']), (3, 1, 2, 3))
        self.assertAlmostEqual(r1['dismissal_rate'], 2 / 3)
        self.assertEqual((r2['flagged'], r2['confirmed'], r2['dismissed']), (1, 1, 0))
        self.assertEqual(set(rep['rules']), {'R001', 'R002'})          # a rule nobody flagged does not appear

    def test_a_later_decision_replaces_an_earlier_one_for_the_same_rule(self):
        decided(self.store, 'C1', {'R001': ('FAIL', 'high')}, {'R001': 'dismiss_with_reason'}, extra=[('R001', 'confirm_issue')])
        r = feedback.report(self.store)['rules']['R001']
        self.assertEqual((r['confirmed'], r['dismissed']), (0, 1))     # the final word counts once, not both

    def test_information_requests_and_corrections_are_counted_but_do_not_resolve(self):
        decided(self.store, 'C1', {'R001': ('FAIL', 'medium')}, {'R001': 'confirm_issue'},
                extra=[('R001', 'request_information'), ('R001', 'mark_corrected_for_recheck'), ('R001', 'request_information')])
        r = feedback.report(self.store)['rules']['R001']
        self.assertEqual((r['request_information'], r['mark_corrected_for_recheck'], r['confirmed']), (2, 1, 1))

    def test_claims_not_decided_yet_are_left_out(self):
        doc = queue_doc('C1')
        doc['results'] = results_of({'R001': ('FAIL', 'medium')}, 'C1')
        self.store.put_triaged(doc)
        self.store.transition('C1', 1, 'triaged', 'ready', 'system:t', 1000.0)
        self.assertEqual(feedback.report(self.store)['claims_decided'], 0)

    def test_signoff_agreement_and_disagreement(self):
        for i, outcome in enumerate(['agreed', 'agreed', 'agreed', 'tiebreak']):
            decided(self.store, f'S{i}', {'R003': ('FAIL', 'high')}, {'R003': 'confirm_issue'},
                    signoff={'stage': 'done', 'outcome': outcome}, escalated=(outcome == 'tiebreak'))
        rep = feedback.report(self.store)
        self.assertEqual(rep['signoff'], {'countersigned': 4, 'agreed': 3, 'disagreed': 1, 'disagreement_rate': 0.25})
        self.assertEqual(rep['escalated'], 1)

    def test_a_small_sample_is_never_a_review_candidate(self):
        for i in range(10):
            decided(self.store, f'D{i}', {'R004': ('FAIL', 'medium')}, {'R004': 'dismiss_with_reason'})
        r = feedback.report(self.store)['rules']['R004']
        self.assertEqual((r['dismissed'], r['dismissal_rate']), (10, 1.0))
        self.assertFalse(r['review_candidate'])                         # 10 < min_decisions, however extreme the rate

    def test_a_rule_people_keep_dismissing_becomes_a_candidate_and_a_balanced_one_does_not(self):
        for i in range(25):
            decided(self.store, f'D{i}', {'R004': ('FAIL', 'medium')}, {'R004': 'dismiss_with_reason'})
        for i in range(25):
            decided(self.store, f'K{i}', {'R005': ('FAIL', 'medium')},
                    {'R005': 'dismiss_with_reason' if i % 2 else 'confirm_issue'})
        rules = feedback.report(self.store)['rules']
        self.assertTrue(rules['R004']['review_candidate'])
        self.assertGreater(rules['R004']['dismissal_rate_lower'], 0.5)
        self.assertFalse(rules['R005']['review_candidate'])             # about half and half: the lower bound is below the threshold

    def test_the_report_carries_no_reason_text_no_ids_and_changes_nothing(self):
        decided(self.store, 'C-SECRET-1', {'R001': ('FAIL', 'medium')}, {'R001': 'dismiss_with_reason'}, reason=SECRET_REASON)
        before = copy.deepcopy(self.store.get('C-SECRET-1', 1))
        counts = dict(self.store.counts())
        text = json.dumps(feedback.report(self.store))
        for needle in ('Maha', 'phoned', 'M-P1', 'C-SECRET-1', 'CG-2002', 'P1'):
            self.assertNotIn(needle, text)
        self.assertEqual(self.store.get('C-SECRET-1', 1), before)
        self.assertEqual(self.store.counts(), counts)
        self.assertEqual(feedback.report(self.store)['rules']['R001']['dismissals_with_reason'], 1)

    def test_a_dismissal_with_no_reason_is_not_counted_as_having_one(self):
        decided(self.store, 'C1', {'R001': ('FAIL', 'medium')}, {'R001': 'dismiss_with_reason'}, reason='   ')
        self.assertEqual(feedback.report(self.store)['rules']['R001']['dismissals_with_reason'], 0)

    def test_bad_parameters_are_refused(self):
        for kw in ({'min_decisions': 0}, {'min_decisions': True}, {'min_decisions': 2.5}, {'threshold': -0.1}, {'threshold': 1.5},
                   {'threshold': True}, {'threshold': 'x'}):
            with self.assertRaises(ValueError, msg=str(kw)):
                feedback.report(self.store, **kw)


for _name, _factory in store_makers():
    globals()[f'Feedback_{_name}'] = type(f'Feedback_{_name}', (FeedbackBase, unittest.TestCase), {'factory': staticmethod(_factory)})

if __name__ == '__main__':
    unittest.main()
