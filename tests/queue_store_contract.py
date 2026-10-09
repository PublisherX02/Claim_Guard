"""One contract for every QueueStore. The in-memory twin and the MongoDB store must both pass it, so the twin is checked against
the real database's behaviour. Races use real threads: the guarantees (one lease, one transition, one config version) must hold
there, not just in sequential code."""
import copy
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from workqueue import states

NOW = 1000.0


def make_doc(claim_id='C1', version=1, input_hash='h1', lane='A', eligibility='decide', score=2, patient='P1', created=NOW, pack=None):
    doc = {
        'claim_id': claim_id, 'version': version, 'input_hash': input_hash,
        'claim': {'claim_id': claim_id, 'patient_id': patient, 'provider_id': 'V1', 'submission_date': '2026-03-10',
                  'lines': [{'line_id': 'L1', 'service_code': 'SVC-LAB', 'quantity': 1}], 'authorizations': [], 'notes': 'n',
                  'diagnosis_code': 'DX-EDU-01', 'attachments': [], 'member_id': 'M1'},
        'results': [{'rule_id': 'R001', 'status': 'FAIL', 'severity': 'high'}],
        'receipt': {'claim_id': claim_id, 'input_hash': input_hash, 'lane': lane, 'eligibility': eligibility, 'score': score,
                    'config_version': 1, 'created_at': created, 'statuses': {'R001': 'FAIL'}},
    }
    if pack is not None:
        doc['receipt']['rule_pack_hash'] = pack
    return doc


def race(count, fn):
    """Run fn(i) in `count` threads released together; return the list of results (exceptions are returned, not raised)."""
    barrier, out = threading.Barrier(count), [None] * count

    def run(i):
        barrier.wait()
        try:
            out[i] = fn(i)
        except Exception as e:  # noqa: BLE001 - the test inspects it
            out[i] = e
    threads = [threading.Thread(target=run, args=(i,)) for i in range(count)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return out


class StoreContract:
    """Mixed into a unittest.TestCase that provides make_store()."""

    def make_store(self):
        raise NotImplementedError

    def setUp(self):
        self.store = self.make_store()

    def ready(self, claim_id='C1', version=1, **kw):
        """Put a claim and walk it to `ready`."""
        self.assertTrue(self.store.put_triaged(make_doc(claim_id, version, **kw)))
        self.assertIsNotNone(self.store.transition(claim_id, version, 'triaged', 'explanation_skipped', 'system:t', NOW + 1))
        return self.store.transition(claim_id, version, 'explanation_skipped', 'ready', 'system:t', NOW + 2)

    # ---- intake and reads
    def test_put_triaged_stores_the_whole_document_in_state_triaged(self):
        self.assertTrue(self.store.put_triaged(make_doc()))
        d = self.store.get('C1')
        self.assertEqual((d['state'], d['state_at'], d['enqueue_pending'], d['lease'], d['decided_by']), ('triaged', NOW, True, None, None))
        self.assertEqual(d['claim']['patient_id'], 'P1')
        self.assertEqual(len(d['events']), 1)
        self.assertEqual((d['events'][0]['from'], d['events'][0]['to']), ('received', 'triaged'))

    def test_returned_documents_are_copies(self):
        self.store.put_triaged(make_doc())
        d = self.store.get('C1')
        d['claim']['patient_id'] = 'HACKED'; d['events'].clear(); d['state'] = 'decided'
        again = self.store.get('C1')
        self.assertEqual((again['claim']['patient_id'], again['state'], len(again['events'])), ('P1', 'triaged', 1))

    def test_stored_documents_do_not_alias_the_input(self):
        doc = make_doc()
        self.store.put_triaged(doc)
        doc['claim']['patient_id'] = 'HACKED'
        self.assertEqual(self.store.get('C1')['claim']['patient_id'], 'P1')

    def test_the_same_claim_and_input_hash_is_stored_once(self):
        self.assertTrue(self.store.put_triaged(make_doc()))
        self.assertFalse(self.store.put_triaged(make_doc()))
        self.assertEqual(self.store.counts(), {'triaged|A|decide': 1})

    def test_a_changed_input_is_the_next_version_and_get_returns_the_latest(self):
        self.store.put_triaged(make_doc(input_hash='h1'))
        self.assertTrue(self.store.put_triaged(make_doc(version=2, input_hash='h2')))
        self.assertEqual(self.store.get('C1')['version'], 2)
        self.assertEqual(self.store.get('C1', 1)['input_hash'], 'h1')
        self.assertIsNone(self.store.get('C1', 3))
        self.assertIsNone(self.store.get('NOPE'))

    def test_the_same_version_with_a_different_input_is_refused(self):
        self.store.put_triaged(make_doc(input_hash='h1'))
        self.assertFalse(self.store.put_triaged(make_doc(input_hash='h2')))
        self.assertEqual(self.store.get('C1')['input_hash'], 'h1')

    def test_the_same_body_under_a_new_rule_pack_is_the_next_version_but_under_the_same_pack_it_is_a_duplicate(self):
        self.assertTrue(self.store.put_triaged(make_doc(pack='p1')))
        self.assertFalse(self.store.put_triaged(make_doc(version=2, pack='p1')))        # same body, same pack: a duplicate
        self.assertTrue(self.store.put_triaged(make_doc(version=2, pack='p2')))         # same body, new pack: version 2
        self.assertFalse(self.store.put_triaged(make_doc(version=3, pack='p2')))
        self.assertFalse(self.store.put_triaged(make_doc(version=2, input_hash='other', pack='p3')))   # the version number is taken
        self.assertEqual([self.store.get('C1', v)['receipt']['rule_pack_hash'] for v in (1, 2)], ['p1', 'p2'])

    def test_advisory_results_are_stored_beside_the_official_ones_and_default_to_none(self):
        self.store.put_triaged(make_doc('C1'))
        self.assertEqual(self.store.get('C1')['advisory'], [])
        with_advice = make_doc('C2'); with_advice['advisory'] = [{'rule_id': 'E101', 'status': 'PASS'}]
        self.assertTrue(self.store.put_triaged(with_advice))
        self.assertEqual(self.store.get('C2')['advisory'], [{'rule_id': 'E101', 'status': 'PASS'}])
        bad = make_doc('C3'); bad['advisory'] = {'rule_id': 'E101'}
        with self.assertRaises(ValueError):
            self.store.put_triaged(bad)

    def test_parallel_intake_of_one_claim_stores_it_once(self):
        wins = [r for r in race(30, lambda i: self.store.put_triaged(make_doc())) if r is True]
        self.assertEqual(len(wins), 1)

    def test_a_malformed_document_is_refused(self):
        bad = make_doc(); del bad['receipt']
        extra = make_doc(); extra['state'] = 'decided'
        wrong_version = make_doc(version=0)
        bool_version = make_doc(); bool_version['version'] = True
        for doc in (bad, extra, wrong_version, bool_version, 'x', None):
            with self.assertRaises((ValueError, TypeError), msg=repr(doc)[:40]):
                self.store.put_triaged(doc)
        self.assertEqual(self.store.counts(), {})

    # ---- transitions
    def test_a_transition_from_the_right_state_appends_exactly_one_event(self):
        self.store.put_triaged(make_doc())
        d = self.store.transition('C1', 1, 'triaged', 'ready', 'system:t', NOW + 5, detail={'why': 'green'})
        self.assertEqual((d['state'], d['state_at']), ('ready', NOW + 5))
        self.assertEqual(len(d['events']), 2)
        self.assertEqual(d['events'][-1], {'from': 'triaged', 'to': 'ready', 'actor': 'system:t', 'at': NOW + 5, 'detail': {'why': 'green'}})

    def test_a_transition_from_the_wrong_state_changes_nothing(self):
        self.store.put_triaged(make_doc())
        before = self.store.get('C1')
        self.assertIsNone(self.store.transition('C1', 1, 'ready', 'leased', 'B1', NOW + 1))
        self.assertEqual(self.store.get('C1'), before)

    def test_an_illegal_pair_raises_and_a_missing_claim_is_none(self):
        self.store.put_triaged(make_doc())
        with self.assertRaises(states.IllegalTransition):
            self.store.transition('C1', 1, 'triaged', 'decided', 'B1', NOW + 1)
        self.assertIsNone(self.store.transition('NOPE', 1, 'triaged', 'ready', 'system:t', NOW + 1))

    def test_set_fields_are_applied_with_the_move_and_unknown_names_are_refused(self):
        self.store.put_triaged(make_doc())
        d = self.store.transition('C1', 1, 'triaged', 'explained', 'system:t', NOW + 1,
                                  set_fields={'explanation': {'text': 'because', 'source': 'ai'}, 'enqueue_pending': False})
        self.assertEqual((d['explanation']['text'], d['enqueue_pending']), ('because', False))
        for name in ('state', 'claim', 'results', 'receipt', 'events', 'version', 'claim_id', '$set', 'lease.badge_id'):
            with self.assertRaises(ValueError, msg=name):
                self.store.transition('C1', 1, 'explained', 'ready', 'system:t', NOW + 2, set_fields={name: 1})
        self.assertEqual(self.store.get('C1')['state'], 'explained')

    def test_a_decision_and_a_hand_back_race_has_exactly_one_winner_and_a_consistent_document(self):
        self.ready()
        self.store.lease('C1', 1, 'B0', NOW + 3, NOW + 100)

        def act(i):
            if i % 2:
                return self.store.transition('C1', 1, 'leased', 'decided', f'B{i}', NOW + 4, set_fields={'decided_by': f'B{i}'})
            return self.store.transition('C1', 1, 'leased', 'ready', 'system:expiry', NOW + 4, set_fields={'lease': None})
        winners = [r for r in race(50, act) if isinstance(r, dict)]
        self.assertEqual(len(winners), 1)
        final = self.store.get('C1')
        self.assertEqual(final['state'], winners[0]['state'])
        self.assertEqual(len(final['events']), 5)       # triaged, explanation_skipped, ready, leased, and exactly one more
        if final['state'] == 'decided':
            self.assertEqual(final['decided_by'], final['events'][-1]['actor'])
        else:
            self.assertIsNone(final['lease'])

    # ---- outbox
    def test_the_outbox_lists_pending_claims_oldest_first_and_clears_once(self):
        self.store.put_triaged(make_doc('C2', created=NOW + 10)); self.store.put_triaged(make_doc('C1', created=NOW))
        self.assertEqual([d['claim_id'] for d in self.store.pending_outbox(10)], ['C1', 'C2'])
        self.assertEqual(len(self.store.pending_outbox(1)), 1)
        self.assertTrue(self.store.clear_outbox('C1', 1))
        self.assertFalse(self.store.clear_outbox('C1', 1))
        self.assertEqual([d['claim_id'] for d in self.store.pending_outbox(10)], ['C2'])

    # ---- queues and leases
    def test_by_state_and_counts_match_a_hand_counted_fixture(self):
        self.store.put_triaged(make_doc('C1', lane='green', eligibility='decide', score=0))
        self.store.put_triaged(make_doc('C2', lane='B', eligibility='decide_high', score=12))
        self.store.put_triaged(make_doc('C3', lane='B', eligibility='decide_high', score=12))
        self.store.transition('C3', 1, 'triaged', 'ready', 'system:t', NOW + 1)
        self.assertEqual(self.store.counts(), {'triaged|green|decide': 1, 'triaged|B|decide_high': 1, 'ready|B|decide_high': 1})
        self.assertEqual([d['claim_id'] for d in self.store.by_state('ready')], ['C3'])
        self.assertEqual(self.store.by_state('decided'), [])
        self.assertEqual(len(self.store.by_state('triaged', limit=1)), 1)

    def test_lease_moves_a_ready_claim_to_one_badge(self):
        self.ready()
        d = self.store.lease('C1', 1, 'B1', NOW + 3, NOW + 100)
        self.assertEqual(d['state'], 'leased')
        self.assertEqual(d['lease'], {'badge_id': 'B1', 'leased_at': NOW + 3, 'expires_at': NOW + 100, 'heartbeat_at': NOW + 3})
        self.assertEqual([x['claim_id'] for x in self.store.inbox('B1')], ['C1'])
        self.assertEqual(self.store.inbox('B2'), [])

    def test_lease_of_a_claim_that_is_not_ready_is_none(self):
        self.store.put_triaged(make_doc())
        self.assertIsNone(self.store.lease('C1', 1, 'B1', NOW, NOW + 100))
        self.assertIsNone(self.store.lease('NOPE', 1, 'B1', NOW, NOW + 100))

    def test_fifty_agents_racing_for_one_claim_give_exactly_one_lease(self):
        self.ready()
        wins = [r for r in race(50, lambda i: self.store.lease('C1', 1, f'B{i}', NOW + 3, NOW + 100)) if isinstance(r, dict)]
        self.assertEqual(len(wins), 1)
        holders = [b for b in (f'B{i}' for i in range(50)) if self.store.inbox(b)]
        self.assertEqual(holders, [wins[0]['lease']['badge_id']])

    def test_inbox_is_ordered_by_lease_time(self):
        for i, cid in enumerate(('C1', 'C2', 'C3')):
            self.ready(cid)
        self.store.lease('C2', 1, 'B1', NOW + 10, NOW + 100); self.store.lease('C1', 1, 'B1', NOW + 20, NOW + 100)
        self.store.lease('C3', 1, 'B1', NOW + 15, NOW + 100)
        self.assertEqual([d['claim_id'] for d in self.store.inbox('B1')], ['C2', 'C3', 'C1'])

    def test_heartbeat_extends_only_that_badges_leases(self):
        self.ready('C1'); self.ready('C2')
        self.store.lease('C1', 1, 'B1', NOW + 3, NOW + 100); self.store.lease('C2', 1, 'B2', NOW + 3, NOW + 100)
        self.assertEqual(self.store.heartbeat('B1', NOW + 50, NOW + 500), 1)
        self.assertEqual(self.store.get('C1')['lease']['expires_at'], NOW + 500)
        self.assertEqual(self.store.get('C1')['lease']['heartbeat_at'], NOW + 50)
        self.assertEqual(self.store.get('C2')['lease']['expires_at'], NOW + 100)
        self.assertEqual(self.store.heartbeat('NOBODY', NOW + 50, NOW + 500), 0)

    def test_expired_respects_the_boundary_exactly(self):
        self.ready('C1'); self.store.lease('C1', 1, 'B1', NOW + 3, NOW + 100)
        self.assertEqual(self.store.expired(NOW + 99.999), [])
        self.assertEqual([d['claim_id'] for d in self.store.expired(NOW + 100)], ['C1'])
        self.assertEqual([d['claim_id'] for d in self.store.expired(NOW + 101)], ['C1'])

    # ---- history for the cross-claim rules
    def test_claims_for_patient_returns_the_latest_version_body_only(self):
        self.store.put_triaged(make_doc('C1', patient='P1', input_hash='a'))
        self.store.put_triaged(make_doc('C1', version=2, patient='P1', input_hash='b'))
        self.store.put_triaged(make_doc('C2', patient='P2'))
        rows = self.store.claims_for_patient('P1')
        self.assertEqual([r['claim_id'] for r in rows], ['C1'])
        self.assertEqual(set(rows[0]), {'claim_id', 'patient_id', 'provider_id', 'submission_date', 'lines', 'authorizations',
                                        'notes', 'diagnosis_code', 'attachments'})
        self.assertNotIn('member_id', rows[0])
        self.assertEqual(self.store.claims_for_patient('NOBODY'), [])

    # ---- configuration
    def test_configuration_versions_are_numbered_and_stale_writes_lose(self):
        self.assertIsNone(self.store.latest_config())
        self.assertTrue(self.store.put_config({'version': 1, 'slice_size': 25}, 0))
        self.assertFalse(self.store.put_config({'version': 1, 'slice_size': 99}, 0))     # a stale writer who saw no configuration
        self.assertTrue(self.store.put_config({'version': 2, 'slice_size': 30}, 1))
        self.assertEqual(self.store.latest_config()['slice_size'], 30)
        self.assertEqual([c['version'] for c in self.store.config_history()], [1, 2])

    def test_a_config_document_must_be_the_next_version(self):
        for doc, expected in (({'version': 3}, 0), ({'version': 1}, 1), ({'slice_size': 1}, 0)):
            with self.assertRaises(ValueError, msg=repr(doc)):
                self.store.put_config(doc, expected)

    def test_twenty_writers_at_one_expected_version_give_one_winner(self):
        wins = [r for r in race(20, lambda i: self.store.put_config({'version': 1, 'slice_size': 10 + i}, 0)) if r is True]
        self.assertEqual(len(wins), 1)
        self.assertEqual(len(self.store.config_history()), 1)

    # ---- deals, dead letters, cache, counters
    def test_deals_round_trip_newest_first(self):
        self.store.append_deal({'deal_id': 'D1', 'seed': 1, 'at': NOW}); self.store.append_deal({'deal_id': 'D2', 'seed': 2, 'at': NOW + 1})
        self.assertEqual(self.store.get_deal('D1')['seed'], 1)
        self.assertIsNone(self.store.get_deal('NOPE'))
        self.assertEqual([d['deal_id'] for d in self.store.deals(10)], ['D2', 'D1'])
        with self.assertRaises(ValueError):
            self.store.append_deal({'seed': 3})

    def test_a_dead_letter_can_be_popped_once_only(self):
        self.store.add_dead_letter({'dead_id': 'X1', 'claim_id': 'C1', 'reason': 'boom', 'at': NOW})
        self.assertEqual([d['dead_id'] for d in self.store.dead_letters()], ['X1'])
        self.assertEqual(self.store.pop_dead_letter('X1')['reason'], 'boom')
        self.assertIsNone(self.store.pop_dead_letter('X1'))
        self.assertEqual(self.store.dead_letters(), [])

    def test_a_store_answers_a_ping(self):
        self.assertTrue(self.store.ping())

    def test_job_records_count_runs_and_failures_and_keep_the_last_success(self):
        self.assertEqual(self.store.jobs(), [])
        self.store.record_job('relay', NOW, True, 'published 2')
        self.store.record_job('relay', NOW + 10, False, 'broker down')
        self.store.record_job('deal', NOW + 5, True)
        jobs = {j['name']: j for j in self.store.jobs()}
        self.assertEqual(sorted(jobs), ['deal', 'relay'])
        self.assertEqual((jobs['relay']['runs'], jobs['relay']['failures'], jobs['relay']['ok']), (2, 1, False))
        self.assertEqual((jobs['relay']['at'], jobs['relay']['last_ok_at']), (NOW + 10, NOW))
        self.assertEqual((jobs['deal']['runs'], jobs['deal']['failures'], jobs['deal']['ok']), (1, 0, True))

    def test_job_records_refuse_malformed_input(self):
        for args in (('', NOW, True), ('x', 'now', True), ('x', NOW, 'yes'), ('x', NOW, True, 'd' * 201), (7, NOW, True)):
            with self.assertRaises((ValueError, TypeError)):
                self.store.record_job(*args)

    def test_the_cache_round_trips_text(self):
        self.assertIsNone(self.store.cache_get('k'))
        self.store.cache_put('k', 'template text')
        self.assertEqual(self.store.cache_get('k'), 'template text')
        self.store.cache_put('k', 'newer')
        self.assertEqual(self.store.cache_get('k'), 'newer')

    def test_bump_allows_exactly_the_limit_across_threads_and_windows_are_independent(self):
        allowed = [r for r in race(50, lambda i: self.store.bump('ai_minute', 'w1', 20)) if r is True]
        self.assertEqual(len(allowed), 20)
        self.assertFalse(self.store.bump('ai_minute', 'w1', 20))
        self.assertTrue(self.store.bump('ai_minute', 'w2', 20))
        self.assertTrue(self.store.bump('other', 'w1', 1))
        self.assertFalse(self.store.bump('other', 'w1', 1))
        self.assertFalse(self.store.bump('zero', 'w', 0))

    # ---- decisions and the holder guard
    def leased(self, badge='B1', expires=NOW + 100):
        self.ready()
        return self.store.lease('C1', 1, badge, NOW + 3, expires)

    def test_a_decision_is_recorded_only_for_the_holder_of_an_unexpired_lease(self):
        self.leased()
        decision = {'rule_id': 'R001', 'action': 'confirm_issue', 'actor': 'B1', 'at': NOW + 4}
        self.assertIsNone(self.store.add_decision('C1', 1, 'B2', NOW + 4, decision))                  # someone else's claim
        self.assertIsNone(self.store.add_decision('C1', 1, 'B1', NOW + 100, decision))                # the lease has run out (<=)
        self.assertIsNone(self.store.add_decision('NOPE', 1, 'B1', NOW + 4, decision))
        self.assertEqual(self.store.get('C1')['decisions'], [])
        doc = self.store.add_decision('C1', 1, 'B1', NOW + 99.9, decision)
        self.assertEqual(doc['decisions'], [decision])
        self.assertEqual(self.store.get('C1')['decisions'], [decision])
        self.store.add_decision('C1', 1, 'B1', NOW + 5, dict(decision, action='dismiss_with_reason'))
        self.assertEqual([d['action'] for d in self.store.get('C1')['decisions']], ['confirm_issue', 'dismiss_with_reason'])

    def test_a_decision_is_refused_when_the_claim_is_not_leased(self):
        self.ready()
        self.assertIsNone(self.store.add_decision('C1', 1, 'B1', NOW + 4, {'rule_id': 'R001'}))
        self.store.put_triaged(make_doc('C2'))
        self.assertIsNone(self.store.add_decision('C2', 1, 'B1', NOW + 4, {'rule_id': 'R001'}))

    def test_a_malformed_decision_is_refused(self):
        self.leased()
        for bad in (None, {}, 'x', ['a'], {'a': object()}, {'a': {'b': {'c': {'d': 1}}}}, {'a': float('nan')}):
            with self.assertRaises((ValueError, TypeError), msg=repr(bad)):
                self.store.add_decision('C1', 1, 'B1', NOW + 4, bad)
        self.assertEqual(self.store.get('C1')['decisions'], [])

    def test_fifty_racing_decisions_against_one_expiry_leave_a_consistent_document(self):
        self.leased()

        def act(i):
            if i % 2:
                return self.store.add_decision('C1', 1, 'B1', NOW + 4, {'rule_id': 'R001', 'n': i})
            return self.store.transition('C1', 1, 'leased', 'ready', 'system:expiry', NOW + 4, set_fields={'lease': None})
        race(50, act)
        final = self.store.get('C1')
        recorded = len(final['decisions'])
        self.assertEqual(final['state'], 'ready')           # exactly one expiry won, then no decision could follow it
        self.assertTrue(all(d['rule_id'] == 'R001' for d in final['decisions']))
        self.assertLessEqual(recorded, 25)
        self.assertEqual(len(final['events']), 5)

    def test_a_shadow_prediction_is_stored_once_and_changes_nothing_else(self):
        self.store.put_triaged(make_doc())
        before = self.store.get('C1')
        self.assertTrue(self.store.set_shadow('C1', 1, {'predicted_clear': 1.0, 'at': NOW}))
        self.assertFalse(self.store.set_shadow('C1', 1, {'predicted_clear': 0.0, 'at': NOW + 1}))
        self.assertFalse(self.store.set_shadow('NOPE', 1, {'predicted_clear': 1.0}))
        after = self.store.get('C1')
        self.assertEqual(after['shadow'], {'predicted_clear': 1.0, 'at': NOW})
        after['shadow'] = None
        self.assertEqual(after, before)
        for bad in (None, {}, 'x', [1]):
            with self.assertRaises((ValueError, TypeError)):
                self.store.set_shadow('C1', 1, bad)

    def test_the_light_queries_agree_with_the_full_documents(self):
        for cid, patient in (('C1', 'P1'), ('C2', 'P1'), ('C3', 'P2')):
            self.ready(cid, score=3 if cid != 'C3' else 7, eligibility='decide_high' if cid == 'C2' else 'decide')
        self.store.put_triaged(make_doc('C1', version=2, input_hash='h2'))
        self.store.lease('C1', 1, 'B1', NOW + 3, NOW + 100)
        self.store.lease('C2', 1, 'B1', NOW + 3, NOW + 100)
        self.store.lease('C3', 1, 'B2', NOW + 3, NOW + 100)
        self.store.transition('C2', 1, 'leased', 'ready', 'system:t', NOW + 4, set_fields={'lease': None, 'escalated': True})
        self.store.lease('C2', 1, 'B2', NOW + 5, NOW + 100)
        self.assertEqual(self.store.latest_versions(['C1', 'C2', 'NOPE']), {'C1': 2, 'C2': 1})
        self.assertEqual(self.store.latest_versions([]), {})
        rows = sorted(self.store.leased_summary(), key=lambda r: r['claim_id'])
        self.assertEqual(rows, [{'claim_id': 'C1', 'version': 1, 'badge_id': 'B1', 'eligibility': 'decide', 'escalated': False, 'signoff': False},
                                {'claim_id': 'C2', 'version': 1, 'badge_id': 'B2', 'eligibility': 'decide_high', 'escalated': True, 'signoff': False},
                                {'claim_id': 'C3', 'version': 1, 'badge_id': 'B2', 'eligibility': 'decide', 'escalated': False, 'signoff': False}])
        self.assertEqual(self.store.inbox_load('B1'), {'count': 1, 'points': 3})
        self.assertEqual(self.store.inbox_load('B2'), {'count': 2, 'points': 10})
        self.assertEqual(self.store.inbox_load('NOBODY'), {'count': 0, 'points': 0})
        for b in ('B1', 'B2'):
            held = self.store.inbox(b)
            self.assertEqual(self.store.inbox_load(b), {'count': len(held), 'points': sum(d['receipt']['score'] for d in held)})

    def test_a_claim_waiting_for_a_countersignature_can_be_leased_to_someone_else_and_the_event_names_its_real_state(self):
        self.leased('B1')
        sign = {'stage': 'countersign', 'first_by': 'B1', 'actions': {'R001': 'confirm_issue'}}
        done = self.store.transition('C1', 1, 'leased', 'awaiting_countersign', 'B1', NOW + 5, set_fields={'lease': None, 'signoff': sign}, holder='B1')
        self.assertEqual((done['state'], done['lease'], done['signoff']), ('awaiting_countersign', None, sign))
        self.assertEqual(self.store.inbox('B1'), [])
        self.assertEqual(self.store.counts(), {'awaiting_countersign|A|decide': 1})
        again = self.store.lease('C1', 1, 'B2', NOW + 6, NOW + 100)
        self.assertEqual((again['state'], again['lease']['badge_id']), ('leased', 'B2'))
        last = again['events'][-1]
        self.assertEqual((last['from'], last['to'], last['actor']), ('awaiting_countersign', 'leased', 'B2'))
        self.assertEqual(self.store.leased_summary()[0]['signoff'], True)

    def test_only_a_ready_or_waiting_claim_can_be_leased(self):
        self.leased('B1')
        self.store.transition('C1', 1, 'leased', 'decided', 'B1', NOW + 5, set_fields={'decided_by': 'B1'})
        self.assertIsNone(self.store.lease('C1', 1, 'B2', NOW + 6, NOW + 100))

    def test_a_transition_with_a_holder_only_applies_to_that_holders_lease(self):
        self.leased('B1')
        self.assertIsNone(self.store.transition('C1', 1, 'leased', 'decided', 'B2', NOW + 5, set_fields={'decided_by': 'B2'}, holder='B2'))
        self.assertEqual(self.store.get('C1')['state'], 'leased')
        done = self.store.transition('C1', 1, 'leased', 'decided', 'B1', NOW + 6, set_fields={'decided_by': 'B1'}, holder='B1')
        self.assertEqual((done['state'], done['decided_by']), ('decided', 'B1'))

    def test_a_holder_guard_fails_when_the_claim_was_re_dealt_to_someone_else(self):
        self.leased('B1')
        self.store.transition('C1', 1, 'leased', 'ready', 'system:expiry', NOW + 10, set_fields={'lease': None})
        self.store.lease('C1', 1, 'B2', NOW + 11, NOW + 500)
        self.assertIsNone(self.store.transition('C1', 1, 'leased', 'decided', 'B1', NOW + 12, set_fields={'decided_by': 'B1'}, holder='B1'))
        self.assertEqual(self.store.get('C1')['lease']['badge_id'], 'B2')

    def test_the_escalated_flag_can_be_set_with_a_move_and_starts_false(self):
        self.assertTrue(self.store.put_triaged(make_doc()))
        self.assertIs(self.store.get('C1')['escalated'], False)
        d = self.store.transition('C1', 1, 'triaged', 'ready', 'system:t', NOW + 1, set_fields={'escalated': True})
        self.assertIs(d['escalated'], True)

    # ---- hostile identifiers
    def test_every_method_refuses_a_query_object_or_other_non_text_as_an_identifier(self):
        s = self.store
        for bad in ({'$ne': None}, None, ['C1'], 5):
            calls = [
                lambda: s.get(bad), lambda: s.transition(bad, 1, 'triaged', 'ready', 'a', 1.0),
                lambda: s.transition('C1', 1, bad, 'ready', 'a', 1.0), lambda: s.transition('C1', 1, 'triaged', 'ready', bad, 1.0),
                lambda: s.clear_outbox(bad, 1), lambda: s.by_state(bad), lambda: s.lease(bad, 1, 'B1', 1.0, 2.0),
                lambda: s.lease('C1', 1, bad, 1.0, 2.0), lambda: s.inbox(bad), lambda: s.heartbeat(bad, 1.0, 2.0),
                lambda: s.claims_for_patient(bad), lambda: s.get_deal(bad), lambda: s.pop_dead_letter(bad),
                lambda: s.cache_get(bad), lambda: s.cache_put(bad, 'x'), lambda: s.cache_put('k', bad),
                lambda: s.bump(bad, 'w', 1), lambda: s.bump('c', bad, 1),
                lambda: s.add_decision(bad, 1, 'B1', 1.0, {'a': 1}), lambda: s.add_decision('C1', 1, bad, 1.0, {'a': 1}),
                lambda: s.transition('C1', 1, 'triaged', 'ready', 'a', 1.0, holder=(bad if bad is not None else 7)),
                lambda: s.set_shadow(bad, 1, {'a': 1}), lambda: s.inbox_load(bad), lambda: s.latest_versions([bad]),
            ]
            for i, call in enumerate(calls):
                with self.assertRaises((TypeError, ValueError, states.IllegalTransition), msg=f'{bad!r} call {i}'):
                    call()

    def test_a_version_must_be_a_whole_number_not_a_bool_or_a_query(self):
        for bad in (True, '1', {'$gt': 0}, 1.0, None, 0, -1):
            with self.assertRaises((TypeError, ValueError), msg=repr(bad)):
                self.store.get('C1', bad) if bad is not None else self.store.transition('C1', bad, 'triaged', 'ready', 'a', 1.0)
