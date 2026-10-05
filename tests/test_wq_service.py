"""The queue service: ownership, fresh permissions, lease-bound decisions, escalation, the dashboard and the versioned configuration."""
import json
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access.service import AuthError, Forbidden
from wq_api_world import QueueWorld
from queue_store_contract import race
from wq_world import store_makers
from workqueue import routing_config as rc
from workqueue.service import Conflict, NotFound

RESOLVE = ('confirm_issue', 'Confirmed against the rule text.')


class ServiceBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.q = self.w.queue
        self.w.on_shift('CG-2002', 'CG-2003', 'CG-3003', 'CG-3004', slice_size=4, low_water=1)

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def flagged_ids(self, claim_id):
        return [r['rule_id'] for r in self.w.claims.get(claim_id)[1] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS')]

    def decide_all(self, who, claim_id):
        for rid in self.flagged_ids(claim_id):
            self.q.decide_finding(who, claim_id, rid, 'confirm_issue', 'Confirmed against the rule text.')

    # ---- the agent's inbox
    def test_an_agent_sees_only_their_own_claims(self):
        ids = [self.w.ready(self.w.find('medium')) for _ in range(1)]
        self.w.lease_to(ids[0], 'CG-2002')
        a, b = self.w.principal(badge='CG-2002'), self.w.principal(badge='CG-2003')
        self.assertEqual([v['claim_id'] for v in self.q.inbox(a)], ids)
        self.assertEqual(self.q.inbox(b), [])

    def test_the_inbox_view_is_masked_hides_actions_and_scrubs_the_explanation(self):
        cid = self.w.find('high')
        claim, results = self.w.claims.get(cid)
        raw = claim['patient_id']
        rid = next(r['rule_id'] for r in results if r['severity'] == 'high' and r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'))
        self.w.ready(cid, explanation={'outcome': 'ai_used', 'findings': {rid: {'text': f'Patient {raw} breaks this rule.', 'source': 'ai'}}})
        self.w.lease_to(cid, 'CG-3003')
        self.w.lease_to(cid, 'CG-3003')                       # a second lease attempt on a leased claim is a no-op
        senior = self.q.inbox(self.w.principal(badge='CG-3003'))[0]
        self.assertNotIn(raw, json.dumps(senior))
        finding = next(f for f in senior['findings'] if f['rule_id'] == rid)
        self.assertIn('confirm_issue', finding['allowed_actions'])
        self.assertIn('PAT-', finding['ai_explanation']['text'])
        self.assertEqual(finding['review_state'], 'unreviewed')
        passing = next(f for f in senior['findings'] if f['status'] == 'PASS')
        self.assertNotIn('allowed_actions', passing)
        self.assertNotIn('ai_explanation', passing)

    def test_an_l2_holding_a_high_finding_sees_no_actions_on_it(self):
        cid = self.w.ready(self.w.find('high'))
        self.w.lease_to(cid, 'CG-2002')                      # leased by hand: the dispatcher would not do this
        view = self.q.inbox(self.w.principal(badge='CG-2002'))[0]
        high = [f for f in view['findings'] if f['status'] in ('FAIL', 'UNABLE_TO_ASSESS') and f['severity'] == 'high']
        self.assertTrue(high)
        self.assertTrue(all('allowed_actions' not in f for f in high))

    def test_the_inbox_is_refused_to_someone_who_cannot_decide(self):
        for badge in ('CG-1001', 'CG-4004'):
            with self.assertRaises(Forbidden):
                self.q.inbox(self.w.principal(badge=badge))

    def test_next_gives_an_agent_on_shift_one_more_claim_and_an_off_shift_agent_none(self):
        self.w.ready(self.w.find('medium'))
        view = self.q.next(self.w.principal(badge='CG-2002'))
        self.assertEqual(view['lane'] in ('A', 'B'), True)
        self.assertIsNone(self.q.next(self.w.principal(badge='CG-2002')))          # nothing else ready
        self.w.on_shift('CG-3003')
        self.w.ready(self.w.find('medium'))
        self.assertIsNone(self.q.next(self.w.principal(badge='CG-2002')))          # no longer on shift

    def test_a_heartbeat_extends_the_leases_of_the_caller_only(self):
        a = self.w.ready(self.w.find('medium'))
        self.w.lease_to(a, 'CG-2002', seconds=100)
        self.w.clock.advance(50)
        mine = self.w.principal(badge='CG-2002')
        self.assertEqual(self.q.heartbeat(mine), 1)
        self.assertEqual(self.qstore.get(a)['lease']['expires_at'], self.w.clock() + self.q.routing_config().lease_seconds)
        self.assertEqual(self.q.heartbeat(self.w.principal(badge='CG-2003')), 0)

    # ---- decisions
    def test_deciding_every_flagged_finding_moves_the_claim_to_decided_with_the_session_badge(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        rids = self.flagged_ids(cid)
        for rid in rids[:-1]:
            out = self.q.decide_finding(who, cid, rid, *RESOLVE)
            self.assertEqual(out['claim_state'], 'leased')
        out = self.q.decide_finding(who, cid, rids[-1], *RESOLVE)
        self.assertEqual(out['claim_state'], 'decided')
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['decided_by']), ('decided', 'CG-2002'))
        self.assertEqual([x['actor'] for x in d['decisions']], ['CG-2002'] * len(rids))
        self.assertEqual(len(self.w.review_rows()) >= len(rids), True)
        self.assertEqual(len([e for e in self.w.events() if e['event_type'] == 'decision']), len(rids))

    def test_a_pending_action_does_not_resolve_and_a_resolved_finding_cannot_be_decided_again(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        rid = self.flagged_ids(cid)[0]
        out = self.q.decide_finding(who, cid, rid, 'request_information', 'Need the referral letter.')
        self.assertEqual((out['claim_state'], out['review_state']), ('leased', 'awaiting_follow_up'))
        self.q.decide_finding(who, cid, rid, *RESOLVE)
        with self.assertRaises(Conflict):
            self.q.decide_finding(who, cid, rid, 'dismiss_with_reason', 'Changed my mind.')

    def test_a_medium_claim_can_be_decided_by_an_l2_but_a_high_finding_needs_the_senior_flag(self):
        cid = self.w.ready(self.w.find('high'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        results = self.w.claims.get(cid)[1]
        high = next(r['rule_id'] for r in results if r['severity'] == 'high' and r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'))
        with self.assertRaises(Forbidden):
            self.q.decide_finding(who, cid, high, *RESOLVE)
        self.assertEqual(self.w.decisions(cid), [])
        self.assertEqual(self.w.review_rows(), [])
        self.w.store.update_user('CG-2002', grants=('claims.decide_high',))
        self.q.decide_finding(self.w.principal(badge='CG-2002'), cid, high, *RESOLVE)
        self.assertEqual(len(self.w.decisions(cid)), 1)

    def test_a_claim_leased_to_someone_else_or_never_mine_is_not_found_and_nothing_is_written(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        rid = self.flagged_ids(cid)[0]
        with self.assertRaises(NotFound):
            self.q.decide_finding(self.w.principal(badge='CG-2003'), cid, rid, *RESOLVE)
        with self.assertRaises(NotFound):
            self.q.decide_finding(self.w.principal(badge='CG-2003'), 'NOPE', rid, *RESOLVE)
        self.assertEqual((self.w.decisions(cid), self.w.review_rows()), ([], []))

    def test_an_unknown_rule_and_a_passing_rule_are_refused(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        passing = next(r['rule_id'] for r in self.w.claims.get(cid)[1] if r['status'] == 'PASS')
        with self.assertRaises(NotFound):
            self.q.decide_finding(who, cid, 'R999', *RESOLVE)
        with self.assertRaises(ValueError):
            self.q.decide_finding(who, cid, passing, *RESOLVE)
        with self.assertRaises(ValueError):
            self.q.decide_finding(who, cid, self.flagged_ids(cid)[0], 'approve_claim', 'x')
        with self.assertRaises(ValueError):
            self.q.decide_finding(who, cid, self.flagged_ids(cid)[0], 'confirm_issue', '   ')
        self.assertEqual(self.w.decisions(cid), [])

    # ---- review focus 2: a demotion while a claim sits in the inbox
    def test_demoting_an_agent_with_a_claim_in_the_inbox_blocks_the_decision_and_returns_the_claim_to_the_pool(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        stale = self.w.principal(badge='CG-2002')                      # a session opened before the demotion
        self.w.store.update_user('CG-2002', level=1)
        with self.assertRaises(Forbidden):
            self.q.decide_finding(stale, cid, self.flagged_ids(cid)[0], *RESOLVE)
        self.assertEqual((self.w.decisions(cid), self.w.review_rows(), self.qstore.get(cid)['state']), ([], [], 'leased'))
        self.assertEqual(self.w.dispatcher.expire(), 1)
        self.assertEqual(self.qstore.get(cid)['state'], 'ready')
        self.assertEqual([e['event_type'] for e in self.w.events() if e['event_type'] == 'lease_reclaimed'], ['lease_reclaimed'])
        self.w.dispatcher.deal()
        self.assertEqual(self.qstore.get(cid)['lease']['badge_id'] in ('CG-2003', 'CG-3003', 'CG-3004'), True)

    def test_a_deactivated_agent_cannot_decide_and_loses_the_claim(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        stale = self.w.principal(badge='CG-2002')
        self.w.store.update_user('CG-2002', active=False)
        with self.assertRaises(Forbidden):
            self.q.decide_finding(stale, cid, self.flagged_ids(cid)[0], *RESOLVE)
        self.assertEqual(self.w.dispatcher.expire(), 1)

    # ---- review focus 3: a decision racing the lease expiry
    def test_a_lease_that_was_re_dealt_mid_request_gives_the_first_agent_a_conflict_and_the_second_the_decision(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.on_shift('CG-2002', 'CG-2003')
        self.w.lease_to(cid, 'CG-2002', seconds=100)
        a = self.w.principal(badge='CG-2002')
        self.w.clock.advance(200)
        self.assertEqual(self.w.dispatcher.expire(), 1)
        deal = self.w.dispatcher.deal()
        self.assertEqual([b for _, b, _ in deal.assigned], ['CG-2003'])
        rid = self.flagged_ids(cid)[0]
        with self.assertRaises(Conflict):
            self.q.decide_finding(a, cid, rid, *RESOLVE)
        self.assertEqual(self.w.decisions(cid), [])
        self.q.decide_finding(self.w.principal(badge='CG-2003'), cid, rid, *RESOLVE)
        self.assertEqual([d['actor'] for d in self.w.decisions(cid)], ['CG-2003'])

    def test_a_lease_that_ran_out_but_was_not_yet_reclaimed_is_also_refused(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002', seconds=100)
        who = self.w.principal(badge='CG-2002')
        self.w.clock.advance(100)
        with self.assertRaises(Conflict) as why:
            self.q.decide_finding(who, cid, self.flagged_ids(cid)[0], *RESOLVE)
        self.assertEqual(str(why.exception), 'lease_expired')

    def test_a_lease_lost_between_the_check_and_the_write_is_a_conflict_and_nothing_reaches_the_review_log(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        original = self.qstore.add_decision

        def lose_the_lease_first(*args, **kwargs):
            self.qstore.transition(cid, 1, 'leased', 'ready', 'system:expiry', self.w.clock(), set_fields={'lease': None})
            return original(*args, **kwargs)
        self.qstore.add_decision = lose_the_lease_first
        try:
            with self.assertRaises(Conflict) as why:
                self.q.decide_finding(who, cid, self.flagged_ids(cid)[0], *RESOLVE)
        finally:
            self.qstore.add_decision = original
        self.assertEqual(str(why.exception), 'lease_lost')
        self.assertEqual((self.w.decisions(cid), self.w.review_rows(), self.qstore.get(cid)['state']), ([], [], 'ready'))
        self.assertEqual([e for e in self.w.events() if e['event_type'] == 'decision'], [])

    def test_a_real_race_between_a_decision_and_an_expiry_never_leaves_two_owners_or_a_lost_write(self):
        for round_ in range(10):
            cid = self.w.find('medium')
            self.w.ready(cid)
            self.w.on_shift('CG-2002', 'CG-2003')
            self.w.lease_to(cid, 'CG-2002', seconds=100)
            who = self.w.principal(badge='CG-2002')
            self.w.clock.advance(99.99)
            rid = self.flagged_ids(cid)[0]

            def act(i):
                if i == 0:
                    self.w.clock.advance(0.02)
                    return self.w.dispatcher.expire()
                try:
                    return self.q.decide_finding(who, cid, rid, *RESOLVE)
                except (Conflict, NotFound) as e:
                    return e
            out = race(2, act)
            doc = self.qstore.get(cid)
            decided_by_a = [d for d in doc['decisions'] if d['actor'] == 'CG-2002']
            self.assertLessEqual(len(decided_by_a), 1)
            self.assertEqual(sum(1 for e in doc['events'] if e['to'] == 'leased'), 1)
            if decided_by_a:
                self.assertEqual(len(self.w.decisions(cid)), 1)
            else:
                self.assertTrue(isinstance(out[1], (Conflict, NotFound)))
            self.w.clock.advance(1000)

    # ---- green claims
    def test_verifying_a_green_claim_clears_it_with_the_persons_badge(self):
        cid = self.w.ready(self.w.find('green'))
        self.w.lease_to(cid, 'CG-2002')
        view = self.q.inbox(self.w.principal(badge='CG-2002'))[0]
        self.assertEqual(view['allowed_actions'], ['verify_clear', 'escalate'])
        self.assertEqual(self.q.decide_green(self.w.principal(badge='CG-2002'), cid, 'verify_clear'), {'claim_state': 'decided'})
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['decided_by']), ('decided', 'CG-2002'))
        green = [e for e in self.w.events() if e['event_type'] == 'claim_decided_green']
        self.assertEqual([(e['badge_id'], e['claim_id'], e['action']) for e in green], [('CG-2002', cid, 'verify_clear')])

    def test_escalating_a_green_claim_sends_it_to_a_senior_and_never_back_to_the_escalator(self):
        cid = self.w.ready(self.w.find('green'))
        self.w.on_shift('CG-3003', 'CG-3004', 'CG-2002')
        self.w.lease_to(cid, 'CG-3003')
        self.assertEqual(self.q.decide_green(self.w.principal(badge='CG-3003'), cid, 'escalate'), {'claim_state': 'ready'})
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['escalated'], d['lease']), ('ready', True, None))
        for seed in range(6):
            self.w.dispatcher.rng_seed = seed
            deal = self.w.dispatcher.deal()
            self.assertEqual([b for _, b, _ in deal.assigned], ['CG-3004'])
            self.qstore.transition(cid, 1, 'leased', 'ready', 'system:t', self.w.clock(), set_fields={'lease': None})

    def test_a_claim_with_findings_cannot_be_cleared_in_one_step_and_a_bad_action_is_refused(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        who = self.w.principal(badge='CG-2002')
        with self.assertRaises(ValueError):
            self.q.decide_green(who, cid, 'verify_clear')
        green = self.w.ready(self.w.find('green'))
        self.w.lease_to(green, 'CG-2002')
        with self.assertRaises(ValueError):
            self.q.decide_green(who, green, 'approve')
        with self.assertRaises(Forbidden):
            self.q.decide_green(self.w.principal(badge='CG-4004'), green, 'verify_clear')

    # ---- administration
    def test_the_admin_cannot_decide_anything_but_can_see_the_dashboard_and_the_configuration(self):
        admin = self.w.principal(badge='CG-4004')
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        for call in (lambda: self.q.inbox(admin), lambda: self.q.next(admin), lambda: self.q.heartbeat(admin),
                     lambda: self.q.decide_finding(admin, cid, self.flagged_ids(cid)[0], *RESOLVE),
                     lambda: self.q.decide_green(admin, cid, 'verify_clear')):
            with self.assertRaises(Forbidden):
                call()
        self.assertIn('counts', self.q.dashboard(admin))
        self.assertEqual(self.q.get_config(admin)['stored_version'], 1)

    def test_agents_cannot_read_or_change_the_queue_administration(self):
        for badge in ('CG-2002', 'CG-3003', 'CG-1001'):
            who = self.w.principal(badge=badge)
            with self.assertRaises(Forbidden):
                self.q.dashboard(who)
            with self.assertRaises(Forbidden):
                self.q.get_config(who)
            with self.assertRaises(Forbidden):
                self.q.set_config(who, {'slice_size': 9}, 1)

    def test_a_configuration_change_makes_the_next_version_logs_before_and_after_and_applies_to_the_next_receipt(self):
        admin = self.w.principal(badge='CG-4004')
        new = self.q.set_config(admin, {'lane_b_score': 3, 'on_shift': ['CG-2002', 'CG-3003']}, 1)
        self.assertEqual((new.version, new.lane_b_score, new.on_shift), (2, 3, ('CG-2002', 'CG-3003')))
        self.assertEqual([c['version'] for c in self.qstore.config_history()], [1, 2])
        event = [e for e in self.w.events() if e['event_type'] == 'routing_config_changed'][-1]
        self.assertEqual((event['actor'], event['version']), ('CG-4004', 2))
        self.assertEqual(set(event['before']), {'lane_b_score', 'on_shift'})
        self.assertEqual(json.loads(event['before']['lane_b_score']), 10)
        self.assertEqual(json.loads(event['after']['lane_b_score']), 3)
        self.assertEqual(json.loads(event['after']['on_shift']), ['CG-2002', 'CG-3003'])
        claim, _ = self.w.claims.get(self.w.find('medium'))
        self.assertEqual(self.w.intake.submit(claim)['config_version'], 2)

    def test_an_invalid_configuration_is_refused_whole_and_writes_no_version(self):
        admin = self.w.principal(badge='CG-4004')
        for bad in ({'slice_size': 2, 'low_water': 9}, {'lane_b_score': -1}, {'points': {'FAIL': {'high': -4, 'medium': 2},
                                                                                       'UNABLE_TO_ASSESS': {'high': 2, 'medium': 1}}},
                    {'lease_seconds': 0}, {'on_shift': ['A', 'A']}, {'nope': 1}, {}, 'x'):
            with self.assertRaises((ValueError, TypeError), msg=repr(bad)):
                self.q.set_config(admin, bad, 1)
        self.assertEqual([c['version'] for c in self.qstore.config_history()], [1])
        self.assertEqual(self.q.routing_config().slice_size, 4)

    def test_a_stale_expected_version_is_a_conflict(self):
        admin = self.w.principal(badge='CG-4004')
        self.q.set_config(admin, {'slice_size': 6}, 1)
        with self.assertRaises(Conflict):
            self.q.set_config(admin, {'slice_size': 7}, 1)
        self.assertEqual(self.q.routing_config().slice_size, 6)

    def test_two_administrators_changing_the_configuration_together_get_one_winner(self):
        admin = self.w.principal(badge='CG-4004')
        out = race(8, lambda i: self._try_config(admin, i))
        self.assertEqual(sum(1 for r in out if r == 'ok'), 1)
        self.assertEqual(len(self.qstore.config_history()), 2)

    def _try_config(self, admin, i):
        try:
            self.q.set_config(admin, {'slice_size': 5 + i}, 1)
            return 'ok'
        except Conflict:
            return 'conflict'

    def test_the_dashboard_counts_match_the_store_and_warn_about_a_missing_senior(self):
        self.w.on_shift('CG-2002', 'CG-2003')
        a, b = self.w.ready(self.w.find('high')), self.w.ready(self.w.find('medium'))
        dash = self.q.dashboard(self.w.principal(badge='CG-4004'))
        self.assertEqual(dash['counts'], self.qstore.counts())
        self.assertEqual(dash['waiting']['decide_high'] >= 1, True)
        self.assertEqual(dash['on_shift_senior'], 0)
        self.assertTrue(any('high-severity' in s for s in dash['shortages']))
        self.w.clock.advance(3600)
        self.assertGreaterEqual(self.q.dashboard(self.w.principal(badge='CG-4004'))['oldest_waiting_seconds']['decide_high'], 3600)
        self.w.on_shift('CG-3003')
        dash = self.q.dashboard(self.w.principal(badge='CG-4004'))
        self.assertEqual((dash['on_shift_senior'], dash['shortages']), (1, []))

    def test_nobody_on_shift_is_a_shortage_not_a_crash(self):
        self.w.on_shift()
        self.w.ready(self.w.find('medium'))
        dash = self.q.dashboard(self.w.principal(badge='CG-4004'))
        self.assertEqual(dash['on_shift'], 0)
        self.assertTrue(dash['shortages'])
        self.assertEqual(self.w.dispatcher.deal().assigned, [])


def _make(name, factory):
    return type('Service_' + name, (ServiceBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Service_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
