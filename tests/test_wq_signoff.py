"""Two-person sign-off: a claim with a high-severity finding is signed by one senior, countersigned by a different one, and settled by
a third when the two disagree. Also the multi-finding behaviour the single-finding tests cannot reach."""
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access.service import Forbidden
from wq_api_world import QueueWorld
from wq_world import store_makers
from workqueue import reconcile, shadow
from workqueue.service import Conflict, NotFound

CONFIRM = ('confirm_issue', 'Confirmed against the rule text.')
DISMISS = ('dismiss_with_reason', 'The evidence does not support it.')
A, B, C, D = 'CG-3003', 'CG-3004', 'CG-3005', 'CG-2002'


class SignoffBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.q = self.w.queue
        self.w.on_shift(A, B, C, D, slice_size=6, low_water=1)

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def flagged(self, cid):
        return [r for r in self.w.claims.get(cid)[1] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS')]

    def high_ids(self, cid):
        return [r['rule_id'] for r in self.flagged(cid) if r['severity'] != 'medium']

    def medium_ids(self, cid):
        return [r['rule_id'] for r in self.flagged(cid) if r['severity'] == 'medium']

    def who(self, badge):
        return self.w.principal(badge=badge)

    def resolve_all(self, badge, cid, action=CONFIRM, only=None):
        out = None
        for rid in (only or [r['rule_id'] for r in self.flagged(cid)]):
            out = self.q.decide_finding(self.who(badge), cid, rid, *action)
        return out

    def first_signs(self, cid, badge=A):
        self.w.lease_to(cid, badge)
        return self.resolve_all(badge, cid)

    def deal_to(self, cid, expect):
        """Run the dispatcher until it deals this claim, and check who got it."""
        deal = self.w.dispatcher.deal()
        got = [b for c, b, _ in deal.assigned if c == cid]
        self.assertEqual(got, [expect] if isinstance(expect, str) else expect)
        return got

    # ---- the first signature
    def test_a_claim_with_a_high_finding_is_not_decided_by_one_senior_it_waits_for_a_countersignature(self):
        cid = self.w.ready(self.w.find('single_high'))
        out = self.first_signs(cid)
        self.assertEqual((out['claim_state'], out['recorded']), ('awaiting_countersign', True))
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['lease'], d['decided_by']), ('awaiting_countersign', None, None))
        self.assertEqual((d['signoff']['stage'], d['signoff']['first_by']), ('countersign', A))
        self.assertEqual(d['signoff']['actions'], {rid: 'confirm_issue' for rid in self.high_ids(cid)})
        self.assertEqual(self.q.inbox(self.who(A)), [])
        events = [e for e in self.w.events() if e['event_type'] == 'claim_signoff']
        self.assertEqual([(e['badge_id'], e['stage'], e['outcome']) for e in events], [(A, 'first', 'pending')])
        self.assertEqual(d['events'][-1]['detail']['event'], 'first_signature')

    def test_a_medium_only_claim_needs_one_person_and_is_decided_at_once(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, D)
        out = self.resolve_all(D, cid)
        self.assertEqual(out['claim_state'], 'decided')
        self.assertIsNone(self.qstore.get(cid)['signoff'])
        self.assertEqual([e for e in self.w.events() if e['event_type'] == 'claim_signoff'], [])

    # ---- who may countersign
    def test_the_first_signer_is_never_dealt_the_claim_again(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(A)
        self.assertEqual(self.w.dispatcher.deal().assigned, [])
        self.assertEqual(self.qstore.get(cid)['state'], 'awaiting_countersign')
        self.w.on_shift(A, B)
        self.deal_to(cid, B)
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['lease']['badge_id']), ('leased', B))
        self.assertEqual(d['events'][-1]['from'], 'awaiting_countersign')

    def test_an_l2_without_the_senior_flag_is_never_offered_a_claim_waiting_for_a_countersignature(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(D, A)
        self.assertEqual(self.w.dispatcher.deal().assigned, [])

    def test_even_if_the_first_signer_is_given_the_claim_by_hand_their_decision_is_refused(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.assertIsNotNone(self.qstore.lease(cid, 1, A, self.w.clock(), self.w.clock() + 900))
        with self.assertRaises(Conflict) as why:
            self.q.decide_finding(self.who(A), cid, self.high_ids(cid)[0], *CONFIRM)
        self.assertEqual(str(why.exception), 'same_person')
        self.assertEqual([d['actor'] for d in self.qstore.get(cid)['decisions']], [A])

    def test_the_first_signer_cannot_act_on_the_claim_once_it_has_left_their_inbox(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        with self.assertRaises(Conflict):
            self.q.decide_finding(self.who(A), cid, self.high_ids(cid)[0], *CONFIRM)

    # ---- the countersignature
    def test_the_second_senior_sees_the_findings_but_not_the_first_seniors_answer(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        view = self.q.inbox(self.who(B))[0]
        self.assertEqual(view['signoff_stage'], 'countersign')
        text = json.dumps(view)
        self.assertFalse(A in text, 'the first signer badge must not appear in the view of the second signer')
        self.assertFalse('first_by' in text or 'decisions' in text or 'actions"' in text.replace('allowed_actions', ''))
        finding = next(f for f in view['findings'] if f['rule_id'] in self.high_ids(cid))
        self.assertEqual(finding['review_state'], 'unreviewed')
        self.assertIn('confirm_issue', finding['allowed_actions'])

    def test_agreeing_decides_the_claim_with_both_signers_on_record(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        out = self.resolve_all(B, cid)
        self.assertEqual(out['claim_state'], 'decided')
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['decided_by']), ('decided', B))
        self.assertEqual({k: d['signoff'][k] for k in ('stage', 'first_by', 'second_by', 'outcome')}, {'stage': 'done', 'first_by': A, 'second_by': B, 'outcome': 'agreed'})
        self.assertEqual([(x['actor'], x['round']) for x in d['decisions']], [(A, 1), (B, 2)])
        events = [(e['badge_id'], e['stage'], e['outcome']) for e in self.w.events() if e['event_type'] == 'claim_signoff']
        self.assertEqual(events, [(A, 'first', 'pending'), (B, 'countersign', 'agreed')])

    def test_disagreeing_sends_the_claim_escalated_to_a_third_senior_whose_decision_is_final(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        out = self.resolve_all(B, cid, DISMISS)
        self.assertEqual(out['claim_state'], 'ready')
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['escalated'], d['lease'], d['decided_by']), ('ready', True, None, None))
        self.assertEqual({k: d['signoff'][k] for k in ('stage', 'first_by', 'second_by', 'outcome')}, {'stage': 'tiebreak', 'first_by': A, 'second_by': B, 'outcome': 'disagreed'})
        self.assertEqual(d['events'][-1]['detail'], {'event': 'signoff_disagreed', 'badge_id': B, 'first_by': A})
        self.w.on_shift(A, B)
        self.assertEqual(self.w.dispatcher.deal().assigned, [])                  # neither of the two signers gets it again
        self.w.on_shift(A, B, C, D)
        self.deal_to(cid, C)
        final = self.resolve_all(C, cid, DISMISS)
        self.assertEqual(final['claim_state'], 'decided')
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['decided_by'], d['signoff']['third_by'], d['signoff']['outcome']), ('decided', C, C, 'tiebreak'))
        events = [(e['badge_id'], e['stage'], e['outcome']) for e in self.w.events() if e['event_type'] == 'claim_signoff']
        self.assertEqual(events, [(A, 'first', 'pending'), (B, 'countersign', 'disagreed'), (C, 'tiebreak', 'final')])
        self.assertTrue(shadow.human_cleared(d))                                  # the final word was "dismissed"

    def test_neither_signer_can_be_the_tiebreaker(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        self.resolve_all(B, cid, DISMISS)
        for badge in (A, B):
            self.qstore.lease(cid, 1, badge, self.w.clock(), self.w.clock() + 900)
            with self.assertRaises(Conflict):
                self.q.decide_finding(self.who(badge), cid, self.high_ids(cid)[0], *CONFIRM)
            self.qstore.transition(cid, 1, 'leased', 'ready', 'system:t', self.w.clock(), set_fields={'lease': None})

    def test_with_only_one_senior_on_shift_the_claim_waits_and_the_dashboard_says_so(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(A, D)
        dash = self.q.dashboard(self.who('CG-4004'))
        self.assertEqual(dash['awaiting_countersign'], 1)
        self.assertEqual(dash['waiting']['decide_high'], 1)
        self.assertTrue(any('countersignature' in s for s in dash['shortages']), dash['shortages'])
        self.w.on_shift(A, B)
        dash = self.q.dashboard(self.who('CG-4004'))
        self.assertFalse(any('countersignature' in s for s in dash['shortages']))

    # ---- claims with several findings
    def test_a_partly_decided_claim_stays_leased_and_a_resolved_finding_cannot_be_decided_again(self):
        cid = self.w.ready(self.w.find('multi'))
        self.w.lease_to(cid, A)
        first, second = [r['rule_id'] for r in self.flagged(cid)][:2]
        out = self.q.decide_finding(self.who(A), cid, first, *CONFIRM)
        self.assertEqual(out['claim_state'], 'leased')
        self.assertEqual(self.qstore.get(cid)['state'], 'leased')
        with self.assertRaises(Conflict) as why:
            self.q.decide_finding(self.who(A), cid, first, *DISMISS)
        self.assertEqual(str(why.exception), 'already_decided')
        view = self.q.inbox(self.who(A))[0]
        by_rule = {f['rule_id']: f for f in view['findings']}
        self.assertEqual(by_rule[first]['review_state'], 'resolved')
        self.assertNotIn('allowed_actions', by_rule[first])                       # a resolved finding offers no actions
        self.assertEqual(by_rule[second]['review_state'], 'unreviewed')
        self.assertIn('allowed_actions', by_rule[second])

    def test_a_pending_action_does_not_count_toward_finishing(self):
        cid = self.w.ready(self.w.find('multi'))
        self.w.lease_to(cid, A)
        rids = [r['rule_id'] for r in self.flagged(cid)]
        for rid in rids:
            out = self.q.decide_finding(self.who(A), cid, rid, 'request_information', 'Need more information.')
        self.assertEqual(out['claim_state'], 'leased')

    def test_in_a_mixed_claim_only_the_high_findings_are_countersigned(self):
        cid = self.w.ready(self.w.find('mixed'))
        self.first_signs(cid)
        d = self.qstore.get(cid)
        self.assertEqual(d['state'], 'awaiting_countersign')
        self.assertEqual(set(d['signoff']['actions']), set(self.high_ids(cid)))
        self.w.on_shift(B)
        self.deal_to(cid, B)
        view = self.q.inbox(self.who(B))[0]
        for f in view['findings']:
            if f['rule_id'] in self.medium_ids(cid):
                self.assertEqual(f['review_state'], 'resolved')
                self.assertNotIn('allowed_actions', f)
            elif f['rule_id'] in self.high_ids(cid):
                self.assertEqual(f['review_state'], 'unreviewed')
        with self.assertRaises(ValueError):
            self.q.decide_finding(self.who(B), cid, self.medium_ids(cid)[0], *CONFIRM)
        out = self.resolve_all(B, cid, only=self.high_ids(cid))
        self.assertEqual(out['claim_state'], 'decided')

    def test_a_single_disagreeing_finding_is_enough_to_escalate(self):
        cid = self.w.ready(self.w.find('mixed'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        highs = self.high_ids(cid)
        for rid in highs[:-1]:
            self.q.decide_finding(self.who(B), cid, rid, *CONFIRM)
        out = self.q.decide_finding(self.who(B), cid, highs[-1], *DISMISS)
        self.assertEqual(out['claim_state'], 'ready')
        self.assertEqual(self.qstore.get(cid)['signoff']['stage'], 'tiebreak')

    def test_a_decision_in_the_second_round_cannot_be_repeated_in_that_round(self):
        cid = self.w.ready(self.w.find('mixed'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        rid = self.high_ids(cid)[0]
        self.q.decide_finding(self.who(B), cid, rid, *CONFIRM)
        with self.assertRaises(Conflict) as why:
            self.q.decide_finding(self.who(B), cid, rid, *CONFIRM)
        self.assertEqual(str(why.exception), 'already_decided')

    def test_a_countersign_that_loses_its_lease_half_way_is_finished_by_someone_other_than_the_first_signer(self):
        cid = self.w.ready(self.w.find('mixed'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        highs = self.high_ids(cid)
        self.q.decide_finding(self.who(B), cid, highs[0], *CONFIRM)
        self.w.clock.advance(4000)
        self.assertEqual(self.w.dispatcher.expire(), 1)
        self.assertEqual(self.qstore.get(cid)['state'], 'ready')
        self.w.on_shift(A, B, C)
        self.deal_to(cid, C)                                                     # not A (first signer), and B is avoided while C exists
        out = None
        for rid in highs[1:]:
            out = self.q.decide_finding(self.who(C), cid, rid, *CONFIRM)
        self.assertEqual(out['claim_state'], 'decided')
        d = self.qstore.get(cid)
        self.assertEqual((d['decided_by'], d['signoff']['second_by'], d['signoff']['outcome']), (C, C, 'agreed'))
        self.assertEqual({x['actor'] for x in d['decisions'] if x['round'] == 2}, {B, C})
        self.assertNotIn(A, {x['actor'] for x in d['decisions'] if x['round'] == 2})

    def test_an_l2_with_the_senior_flag_can_countersign_and_one_without_cannot(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.store.update_user('CG-2003', grants=('claims.decide_high',))
        self.w.on_shift('CG-2003')
        self.deal_to(cid, 'CG-2003')
        out = self.resolve_all('CG-2003', cid)
        self.assertEqual(out['claim_state'], 'decided')

    def test_a_demoted_countersigner_loses_the_claim_and_cannot_decide(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        stale = self.who(B)
        self.w.store.update_user(B, level=2)
        with self.assertRaises(Forbidden):
            self.q.decide_finding(stale, cid, self.high_ids(cid)[0], *CONFIRM)
        self.assertEqual(self.w.dispatcher.expire(), 1)
        self.assertEqual(self.qstore.get(cid)['state'], 'ready')
        self.assertEqual(self.qstore.get(cid)['signoff']['stage'], 'countersign')       # still needs a countersignature

    def test_a_claim_that_carries_a_sign_off_record_needs_a_senior_even_if_its_receipt_says_otherwise(self):
        from wq_world import put_ready
        put_ready(self.qstore, 'ODD', now=self.w.clock(), eligibility='decide', lane='A', score=2)
        self.qstore.lease('ODD', 1, A, self.w.clock(), self.w.clock() + 900)
        self.qstore.transition('ODD', 1, 'leased', 'awaiting_countersign', A, self.w.clock(), holder=A,
                               set_fields={'lease': None, 'signoff': {'stage': 'countersign', 'first_by': A, 'actions': {}}})
        self.w.on_shift(D, B)
        deal = self.w.dispatcher.deal()
        self.assertEqual([(c, b) for c, b, _ in deal.assigned], [('ODD', B)])         # the L2 on shift is passed over

    # ---- housekeeping
    def test_reconcile_reports_a_claim_waiting_a_day_for_its_countersignature(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        report = reconcile.reconcile(self.qstore, self.w.clock() + 86401)
        self.assertEqual([(f['check'], f['state']) for f in report.findings if f['claim_id'] == cid], [('waiting_too_long', 'awaiting_countersign')])
        self.assertEqual(reconcile.reconcile(self.qstore, self.w.clock() + 100).findings, [])

    def test_the_final_human_outcome_is_the_last_word(self):
        cid = self.w.ready(self.w.find('single_high'))
        self.first_signs(cid)
        self.w.on_shift(B)
        self.deal_to(cid, B)
        self.resolve_all(B, cid, DISMISS)
        self.w.on_shift(C)
        self.deal_to(cid, C)
        self.resolve_all(C, cid, CONFIRM)
        self.assertFalse(shadow.human_cleared(self.qstore.get(cid)))            # A confirmed, B dismissed, C confirmed: the last word stands


def _make(name, factory):
    return type('Signoff_' + name, (SignoffBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Signoff_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
