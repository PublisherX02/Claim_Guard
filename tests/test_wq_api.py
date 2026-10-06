"""The queue's HTTP routes: the permission matrix, CSRF, strict bodies, no assignment route, 404/409/422 behaviour and masking."""
import json
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from wq_api_world import QueueWorld
from wq_world import store_makers

API = '/api/v1'
RESOLVE = {'action': 'confirm_issue', 'reason': 'Confirmed against the rule text.'}
LEVEL_BADGE = {1: 'CG-1001', 2: 'CG-2002', 3: 'CG-3003', 4: 'CG-4004'}


def flagged(w, cid):
    return [r['rule_id'] for r in w.claims.get(cid)[1] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS')]


class ApiBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.w.on_shift('CG-2002', 'CG-2003', 'CG-3003', 'CG-3004', slice_size=4, low_water=1)

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def leased_medium(self, badge='CG-2002', seconds=1800):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, badge, seconds)
        return cid

    # ---- the permission matrix
    def matrix_rows(self, w):
        med = w.ready(w.find('medium'))
        green = w.ready(w.find('green'))
        rule = flagged(w, med)[0]
        return [
            ('inbox', 'GET', f'{API}/work/inbox', None, None, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
            ('next', 'POST', f'{API}/work/next', None, None, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
            ('heartbeat', 'POST', f'{API}/work/heartbeat', None, None, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
            ('decision', 'POST', f'{API}/work/claims/{med}/findings/{rule}/decision', RESOLVE, med, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
            ('verify', 'POST', f'{API}/work/claims/{green}/verify', {'action': 'verify_clear'}, green, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
            ('dashboard', 'GET', f'{API}/queue/dashboard', None, None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
            ('feedback', 'GET', f'{API}/queue/feedback', None, None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
            ('config get', 'GET', f'{API}/queue/config', None, None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
            ('config patch', 'PATCH', f'{API}/queue/config', {'expected_version': 1, 'slice_size': 6}, None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ]

    def test_the_whole_permission_matrix(self):
        failures = []
        names = [row[0] for row in self.matrix_rows(self.w)]
        for index, name in enumerate(names):
            for caller in (None, 1, 2, 3, 4):
                qstore, cleanup = self.factory()
                w = QueueWorld(qstore)
                try:
                    w.staff()
                    w.on_shift('CG-2002', 'CG-3003', slice_size=4, low_water=1)
                    _, method, path, body, lease_claim, expected = self.matrix_rows(w)[index]
                    if lease_claim and caller in (2, 3):
                        w.lease_to(lease_claim, LEVEL_BADGE[caller])
                    client = w.client() if caller is None else w.login_client(badge=LEVEL_BADGE[caller])
                    r = client.request(method, path, **({'json': body} if body is not None else {}))
                    if r.status_code != expected[caller]:
                        failures.append(f'{name} as {caller}: expected {expected[caller]}, got {r.status_code} {r.text[:120]}')
                finally:
                    w.close()
                    cleanup()
        self.assertEqual(failures, [])

    # ---- the request layer
    def test_csrf_protects_every_unsafe_queue_route(self):
        cid = self.leased_medium()
        rule = flagged(self.w, cid)[0]
        c = self.w.login_client(badge='CG-2002')
        c.headers.pop('X-CSRF-Token')
        for path, body in ((f'{API}/work/next', None), (f'{API}/work/heartbeat', None),
                           (f'{API}/work/claims/{cid}/findings/{rule}/decision', RESOLVE),
                           (f'{API}/work/claims/{cid}/verify', {'action': 'verify_clear'})):
            r = c.post(path, **({'json': body} if body else {}))
            self.assertEqual((r.status_code, r.json()), (403, {'error': 'csrf'}), path)
        admin = self.w.login_client(badge='CG-4004')
        admin.headers.pop('X-CSRF-Token')
        r = admin.patch(f'{API}/queue/config', json={'expected_version': 1, 'slice_size': 6})
        self.assertEqual((r.status_code, r.json()), (403, {'error': 'csrf'}))
        self.assertEqual(self.qstore.get(cid)['decisions'], [])
        self.assertEqual(len(self.qstore.config_history()), 1)

    def test_unknown_fields_and_wrong_types_are_refused_and_the_client_cannot_choose_the_actor_or_the_state(self):
        cid = self.leased_medium()
        rule = flagged(self.w, cid)[0]
        c = self.w.login_client(badge='CG-2002')
        path = f'{API}/work/claims/{cid}/findings/{rule}/decision'
        for extra in ({'actor': 'CG-3003'}, {'state': 'decided'}, {'original_status': 'PASS'}, {'decided_by': 'CG-3003'},
                      {'lease': None}, {'created_at': '2000-01-01T00:00:00+00:00'}):
            r = c.post(path, json={**RESOLVE, **extra})
            self.assertEqual(r.status_code, 422, extra)
            self.assertEqual(r.json(), {'error': 'invalid_request'})
        for bad in ({'action': 'approve_claim', 'reason': 'x'}, {'action': 'confirm_issue'}, {'action': 'confirm_issue', 'reason': ''},
                    {'action': ['confirm_issue'], 'reason': 'x'}, {'action': 'confirm_issue', 'reason': 'x' * 1001}, [], 'x', None):
            self.assertEqual(c.post(path, json=bad).status_code, 422, bad)
        self.assertEqual(self.qstore.get(cid)['decisions'], [])

    def test_the_decision_is_attributed_to_the_session_not_to_the_body(self):
        cid = self.leased_medium()
        c = self.w.login_client(badge='CG-2002')
        for rule in flagged(self.w, cid):
            r = c.post(f'{API}/work/claims/{cid}/findings/{rule}/decision', json=RESOLVE)
            self.assertEqual(r.status_code, 200, r.text)
        d = self.qstore.get(cid)
        self.assertEqual((d['state'], d['decided_by']), ('decided', 'CG-2002'))
        self.assertEqual({x['actor'] for x in d['decisions']}, {'CG-2002'})
        rows = [json.loads(line)['event'] for line in self.w.review_rows()]
        self.assertEqual({e['actor'] for e in rows}, {'CG-2002'})

    def test_a_claim_that_is_not_yours_is_404_and_a_lost_lease_is_409_and_neither_writes(self):
        cid = self.leased_medium('CG-2002', seconds=100)
        rule = flagged(self.w, cid)[0]
        other = self.w.login_client(badge='CG-2003')
        r = other.post(f'{API}/work/claims/{cid}/findings/{rule}/decision', json=RESOLVE)
        self.assertEqual((r.status_code, r.json()), (404, {'error': 'not_found'}))
        mine = self.w.login_client(badge='CG-2002')
        self.w.on_shift('CG-2002', 'CG-2003')
        self.w.clock.advance(200)
        self.w.dispatcher.expire()
        self.w.dispatcher.deal()
        r = mine.post(f'{API}/work/claims/{cid}/findings/{rule}/decision', json=RESOLVE)
        self.assertEqual((r.status_code, r.json()['error']), (409, 'conflict'))
        self.assertEqual(self.qstore.get(cid)['decisions'], [])
        self.assertEqual(self.w.review_rows(), [])

    def test_a_demoted_agent_gets_403_on_the_next_decision(self):
        cid = self.leased_medium()
        rule = flagged(self.w, cid)[0]
        c = self.w.login_client(badge='CG-2002')
        self.w.store.update_user('CG-2002', level=1)
        r = c.post(f'{API}/work/claims/{cid}/findings/{rule}/decision', json=RESOLVE)
        self.assertEqual(r.status_code, 403)
        self.assertEqual((self.qstore.get(cid)['decisions'], self.w.review_rows()), ([], []))
        self.assertEqual(c.get(f'{API}/work/inbox').status_code, 403)           # the very next request already sees the new role

    def test_nobody_can_assign_or_move_a_particular_claim(self):
        cid = self.leased_medium()
        admin = self.w.login_client(badge='CG-4004')
        senior = self.w.login_client(badge='CG-3003')
        for client in (admin, senior):
            for method, path in (('POST', f'{API}/work/claims/{cid}/assign'), ('POST', f'{API}/queue/claims/{cid}/assign'),
                                 ('PATCH', f'{API}/queue/claims/{cid}'), ('PUT', f'{API}/work/claims/{cid}'),
                                 ('POST', f'{API}/queue/assign'), ('DELETE', f'{API}/work/claims/{cid}'),
                                 ('POST', f'{API}/work/claims/{cid}/lease')):
                r = client.request(method, path, json={'badge_id': 'CG-3003'})
                self.assertIn(r.status_code, (404, 405), (method, path))
                self.assertEqual(set(r.json()), {'error'})
        self.assertEqual(self.qstore.get(cid)['lease']['badge_id'], 'CG-2002')

    # ---- the agent's views
    def test_the_inbox_shows_only_the_callers_claims_masked_with_only_their_actions(self):
        cid = self.w.ready(self.w.find('high'))
        mid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-3003')
        self.w.lease_to(mid, 'CG-2002')
        raw = self.w.claims.get(cid)[0]['patient_id']
        senior = self.w.login_client(badge='CG-3003').get(f'{API}/work/inbox').json()['claims']
        self.assertEqual([v['claim_id'] for v in senior], [cid])
        self.assertNotIn(raw, json.dumps(senior))
        junior = self.w.login_client(badge='CG-2002').get(f'{API}/work/inbox').json()['claims']
        self.assertEqual([v['claim_id'] for v in junior], [mid])
        self.assertNotIn(self.w.claims.get(mid)[0]['patient_id'], json.dumps(junior))
        self.assertTrue(all('allowed_actions' in f for v in junior for f in v['findings'] if f['status'] in ('FAIL', 'UNABLE_TO_ASSESS')))

    def test_next_and_heartbeat_over_http(self):
        self.w.ready(self.w.find('medium'))
        c = self.w.login_client(badge='CG-2002')
        got = c.post(f'{API}/work/next').json()['claim']
        self.assertIsNotNone(got)
        self.assertEqual(c.post(f'{API}/work/next').json(), {'claim': None})
        self.assertEqual(c.post(f'{API}/work/heartbeat').json(), {'extended': 1})

    def test_green_claims_can_be_verified_or_escalated_over_http(self):
        green = self.w.ready(self.w.find('green'))
        self.w.lease_to(green, 'CG-2002')
        c = self.w.login_client(badge='CG-2002')
        self.assertEqual(c.post(f'{API}/work/claims/{green}/verify', json={'action': 'bogus'}).status_code, 422)
        self.assertEqual(c.post(f'{API}/work/claims/{green}/verify', json={'action': 'verify_clear'}).json(), {'claim_state': 'decided'})
        self.assertEqual(c.post(f'{API}/work/claims/{green}/verify', json={'action': 'verify_clear'}).status_code, 409)

    def test_a_finding_that_is_not_flagged_is_a_422_and_an_unknown_one_a_404(self):
        cid = self.leased_medium()
        passing = next(r['rule_id'] for r in self.w.claims.get(cid)[1] if r['status'] == 'PASS')
        c = self.w.login_client(badge='CG-2002')
        self.assertEqual(c.post(f'{API}/work/claims/{cid}/findings/{passing}/decision', json=RESOLVE).status_code, 422)
        self.assertEqual(c.post(f'{API}/work/claims/{cid}/findings/R999/decision', json=RESOLVE).status_code, 404)

    # ---- administration
    def test_a_configuration_change_over_http_creates_the_next_version(self):
        admin = self.w.login_client(badge='CG-4004')
        r = admin.patch(f'{API}/queue/config', json={'expected_version': 1, 'slice_size': 6, 'on_shift': ['CG-2002']})
        self.assertEqual((r.status_code, r.json()), (200, {'version': 2}))
        got = admin.get(f'{API}/queue/config').json()
        self.assertEqual((got['stored_version'], got['config']['slice_size'], got['config']['on_shift']), (2, 6, ['CG-2002']))
        self.assertEqual([c['version'] for c in self.qstore.config_history()], [1, 2])

    def test_a_stale_version_is_409_and_an_invalid_configuration_is_422_and_neither_writes(self):
        admin = self.w.login_client(badge='CG-4004')
        self.assertEqual(admin.patch(f'{API}/queue/config', json={'expected_version': 0, 'slice_size': 6}).status_code, 409)
        for bad in ({'slice_size': 2, 'low_water': 9}, {'lane_b_score': -1}, {'lease_seconds': 0}, {'on_shift': ['A', 'A']},
                    {'points': {'FAIL': {'high': -4, 'medium': 2}, 'UNABLE_TO_ASSESS': {'high': 2, 'medium': 1}}}):
            r = admin.patch(f'{API}/queue/config', json={'expected_version': 1, **bad})
            self.assertEqual(r.status_code, 422, bad)
        for bad in ({'expected_version': 1, 'nope': 1}, {'slice_size': 6}, {'expected_version': 1, 'slice_size': None},
                    {'expected_version': 1}, {'expected_version': -1, 'slice_size': 5}, {'expected_version': '1', 'slice_size': 5}):
            self.assertEqual(admin.patch(f'{API}/queue/config', json=bad).status_code, 422, bad)
        self.assertEqual([c['version'] for c in self.qstore.config_history()], [1])

    def test_the_dashboard_matches_the_store(self):
        self.w.ready(self.w.find('medium'))
        body = self.w.login_client(badge='CG-4004').get(f'{API}/queue/dashboard').json()
        self.assertEqual(body['counts'], self.qstore.counts())
        self.assertEqual(body['config_version'], 1)

    def test_the_feedback_report_reflects_a_real_decision_and_carries_no_reason_text(self):
        cid = self.leased_medium()
        rule = flagged(self.w, cid)[0]
        typed = 'Phoned the member Maha Al-Test about this'
        r = self.w.login_client(badge='CG-2002').post(f'{API}/work/claims/{cid}/findings/{rule}/decision',
                                                        json={'action': 'dismiss_with_reason', 'reason': typed})
        self.assertEqual(r.status_code, 200, r.text)
        resp = self.w.login_client(badge='CG-4004').get(f'{API}/queue/feedback')
        self.assertEqual(resp.status_code, 200)
        body = resp.json()
        self.assertEqual(body['rules'][rule]['dismissed'], 1)
        for needle in (typed, 'Maha', cid, 'CG-2002'):
            self.assertNotIn(needle, resp.text)

    def test_a_refused_request_is_audited(self):
        self.w.login_client(badge='CG-4004').get(f'{API}/work/inbox')
        events = [e for e in self.w.events() if e['event_type'] == 'forbidden']
        self.assertEqual([(e['badge_id'], e['method'], e['path']) for e in events], [('CG-4004', 'GET', f'{API}/work/inbox')])

    def test_errors_are_json_and_leak_nothing(self):
        c = self.w.login_client(badge='CG-2002')
        for r in (c.get(f'{API}/work/claims/%2e%2e%2f/findings/R001/decision'), c.post(f'{API}/work/claims/{"x" * 5000}/verify', json={'action': 'verify_clear'})):
            self.assertIn(r.status_code, (404, 405, 422, 403))
            self.assertNotIn('Traceback', r.text)
            self.assertEqual(set(r.json()), {'error'} | ({'detail'} if 'detail' in r.json() else set()))


def _make(name, factory):
    return type('Api_' + name, (ApiBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['Api_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
