import json
import re
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import masking
from access_api_world import ApiWorld
from access_world import BADGES, PASSWORDS

API = '/api/v1'
NO_SESSION = None


def public_raw_ids(claim):
    return set(re.findall(r'(?:PAT|MEM)-[0-9A-F]{6,}', json.dumps(claim)))


class LoginApiTests(unittest.TestCase):
    def setUp(self):
        self.w = ApiWorld()
        self.w.provision_all()

    def tearDown(self):
        self.w.close()

    def login(self, c, badge, password, code):
        return c.post(f'{API}/auth/login', json={'badge_id': badge, 'password': password, 'totp': code})

    def test_a_good_login_sets_a_hardened_session_cookie_and_a_readable_csrf_cookie(self):
        with ApiWorld(COOKIE_SECURE='true') as w:                      # dev mode, but the cookie flag asked for in production
            w.provision_all()
            r = w.client().post(f'{API}/auth/login', json={'badge_id': BADGES[2], 'password': PASSWORDS[2], 'totp': w.code(BADGES[2])})
            self.assertEqual(r.status_code, 200)
            cookies = {h.split('=', 1)[0]: h for h in r.headers.get_list('set-cookie')}
            session, csrf = cookies['cg_session'], cookies['cg_csrf']
            for flag in ('HttpOnly', 'SameSite=strict'):
                self.assertIn(flag.lower(), session.lower())
            self.assertNotIn('httponly', csrf.lower())
            self.assertIn('samesite=strict', csrf.lower())
            body = r.json()
            self.assertEqual(set(body), {'expires_at', 'csrf', 'must_change_password'})

    def test_me_describes_the_caller_and_nothing_secret(self):
        me = self.w.login_client(3).get(f'{API}/auth/me').json()
        self.assertEqual(set(me), {'badge_id', 'name', 'level', 'permissions', 'must_change_password'})
        self.assertEqual(me['badge_id'], BADGES[3])
        self.assertIn('claims.decide_high', me['permissions'])
        self.assertNotIn('hash', json.dumps(me).lower())

    def test_every_kind_of_failed_login_gets_the_same_response(self):
        w = self.w
        w.provision(2, badge='CG-2999')
        w.store.update_user('CG-2999', active=False)
        w.provision(2, badge='CG-2888')
        for _ in range(5):
            w.store.record_failed_login('CG-2888', w.clock.now, 5, 900)
        attempts = [('CG-7777', 'Whatever-Pass-1!', '123456'), (BADGES[2], 'Wrong-Pass-1357!', w.code(BADGES[2])),
                    (BADGES[2], PASSWORDS[2], '000000'), ('CG-2999', PASSWORDS[2], w.code('CG-2999')),
                    ('CG-2888', PASSWORDS[2], w.code('CG-2888'))]
        seen = set()
        for badge, pw, code in attempts:
            r = self.login(w.client(), badge, pw, code)
            seen.add((r.status_code, r.content, r.headers['content-type'], r.headers['cache-control']))
        self.assertEqual(len(seen), 1, seen)
        self.assertEqual(next(iter(seen))[0], 401)

    def test_malformed_login_bodies_are_rejected_without_echoing_the_input(self):
        c = self.w.client()
        for body in ({'badge_id': {'$ne': None}, 'password': 'x', 'totp': '1'}, {'badge_id': 'CG-2002', 'password': 12345, 'totp': '1'},
                     {'badge_id': 'CG-2002', 'password': 'Planted-Secret-Pass-9!'}, {'badge_id': 'CG-2002', 'password': 'Planted-Secret-Pass-9!', 'totp': '123456', 'extra': 1},
                     [], 'text', None):
            r = c.post(f'{API}/auth/login', json=body)
            self.assertEqual(r.status_code, 422, body)
            self.assertNotIn('Planted-Secret-Pass-9', r.text)
            self.assertEqual(set(r.json()), {'error'})

    def test_twenty_failures_from_one_address_bring_a_429(self):
        c = self.w.client()
        codes = [self.login(c, 'CG-7777', 'Whatever-Pass-1!', '123456').status_code for _ in range(22)]
        self.assertEqual(codes[:20], [401] * 20)
        self.assertEqual(codes[20:], [429, 429])
        r = self.login(c, 'CG-7777', 'Whatever-Pass-1!', '123456')
        self.assertIn('retry-after', r.headers)
        self.w.clock.advance(901)
        self.assertEqual(self.login(c, 'CG-7777', 'Whatever-Pass-1!', '123456').status_code, 401)

    def test_logout_clears_the_cookies_and_ends_the_session(self):
        c = self.w.login_client(2)
        old = c.cookies.get('cg_session')
        r = c.post(f'{API}/auth/logout')
        self.assertEqual(r.status_code, 200)
        other = self.w.client()
        other.cookies.set('cg_session', old)
        self.assertEqual(other.get(f'{API}/auth/me').status_code, 401)

    def test_csrf_protects_every_unsafe_method(self):
        w = self.w
        c = w.login_client(2)
        good = c.headers.pop('X-CSRF-Token')
        self.assertEqual(c.post(f'{API}/auth/logout').status_code, 403)                          # missing
        self.assertEqual(c.post(f'{API}/auth/logout', headers={'X-CSRF-Token': 'wrong'}).status_code, 403)
        other = w.login_client(3)
        self.assertEqual(c.post(f'{API}/auth/logout', headers={'X-CSRF-Token': other.csrf}).status_code, 403)   # another session's token
        self.assertEqual(c.get(f'{API}/auth/me').status_code, 200)                              # reads need none
        self.assertEqual(c.post(f'{API}/auth/logout', headers={'X-CSRF-Token': good}).status_code, 200)

    def test_a_user_who_must_change_their_password_can_do_nothing_else_until_they_do(self):
        w = self.w
        w.provision(2, badge='CG-2777')
        w.store.update_user('CG-2777', must_change_password=True)
        c = w.login_client(2, badge='CG-2777')
        self.assertEqual(c.get(f'{API}/claims').status_code, 403)
        self.assertEqual(c.get(f'{API}/claims').json(), {'error': 'password_change_required'})
        self.assertEqual(c.get(f'{API}/auth/me').status_code, 200)
        r = c.post(f'{API}/auth/change-password', json={'old_password': PASSWORDS[2], 'new_password': 'Brand-New-Pass-2468!'})
        self.assertEqual(r.status_code, 200, r.text)
        c2 = w.login_client(2, badge='CG-2777', password='Brand-New-Pass-2468!')
        self.assertEqual(c2.get(f'{API}/claims').status_code, 200)


def endpoints(w):
    """The whole API as data: (name, method, path, json body, expected status for each caller). One table drives the matrix test."""
    high_claim, high_rule = w.high()
    med_claim, med_rule = w.medium()
    claim = high_claim
    counter = iter(range(5000, 9999))
    def newuser():
        n = next(counter)
        return {'badge_id': f'CG-{n}', 'name': 'Someone', 'password': 'Fresh-Pass-8642!', 'level': 2}
    return [
        ('healthz', 'GET', '/healthz', None, {None: 200, 1: 200, 2: 200, 3: 200, 4: 200}),
        ('me', 'GET', f'{API}/auth/me', None, {None: 401, 1: 200, 2: 200, 3: 200, 4: 200}),
        ('claims list', 'GET', f'{API}/claims', None, {None: 401, 1: 200, 2: 200, 3: 200, 4: 403}),
        ('claim detail', 'GET', f'{API}/claims/{claim}', None, {None: 401, 1: 200, 2: 200, 3: 200, 4: 403}),
        ('unmask', 'POST', f'{API}/claims/{claim}/unmask', {'reason': 'Needed to call the patient'}, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
        ('decision, medium finding', 'POST', f'{API}/claims/{med_claim}/findings/{med_rule}/decision',
         {'action': 'confirm_issue', 'reason': 'Checked the evidence'}, {None: 401, 1: 403, 2: 200, 3: 200, 4: 403}),
        ('decision, high finding', 'POST', f'{API}/claims/{high_claim}/findings/{high_rule}/decision',
         {'action': 'confirm_issue', 'reason': 'Checked the evidence'}, {None: 401, 1: 403, 2: 403, 3: 200, 4: 403}),
        ('recheck', 'POST', f'{API}/claims/{claim}/recheck', {'rule_ids': [high_rule]}, {None: 401, 1: 403, 2: 501, 3: 501, 4: 403}),
        ('audit events', 'GET', f'{API}/audit/events', None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('audit verify', 'GET', f'{API}/audit/verify', None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('users list', 'GET', f'{API}/users', None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('users create', 'POST', f'{API}/users', newuser, {None: 401, 1: 403, 2: 403, 3: 403, 4: 201}),
        ('users patch', 'PATCH', f'{API}/users/{BADGES[1]}', {'name': 'Renamed'}, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('users unlock', 'POST', f'{API}/users/{BADGES[1]}/unlock', None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('users reset totp', 'POST', f'{API}/users/{BADGES[1]}/reset-totp', None, {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
        ('change password (wrong old)', 'POST', f'{API}/auth/change-password', {'old_password': 'Nope-Nope-12345!', 'new_password': 'Another-Pass-2468!'},
         {None: 401, 1: 401, 2: 401, 3: 401, 4: 401}),
        ('logout', 'POST', f'{API}/auth/logout', None, {None: 401, 1: 200, 2: 200, 3: 200, 4: 200}),
    ]


class MatrixTests(unittest.TestCase):
    """Every endpoint against every kind of caller, with the expected status written out. One row per endpoint."""

    def test_the_whole_permission_matrix(self):
        failures = []
        with ApiWorld() as probe:
            names = [e[0] for e in endpoints(probe)]
        for index, name in enumerate(names):
            for caller in (None, 1, 2, 3, 4):
                with ApiWorld() as w:
                    w.provision_all()
                    _, method, path, body, expected = endpoints(w)[index]
                    client = w.client() if caller is None else w.login_client(caller)
                    json_body = body() if callable(body) else body
                    kwargs = {'json': json_body} if json_body is not None else {}
                    r = client.request(method, path, **kwargs)
                    if r.status_code != expected[caller]:
                        failures.append(f'{name} as {caller}: expected {expected[caller]}, got {r.status_code} {r.text[:120]}')
        self.assertEqual(failures, [])

    def test_unknown_routes_and_wrong_methods_are_json_and_leak_nothing(self):
        with ApiWorld() as w:
            w.provision_all()
            c = w.login_client(2)
            for method, path, status in (('GET', '/nope', 404), ('GET', f'{API}/nope', 404), ('DELETE', f'{API}/claims', 405),
                                         ('PUT', f'{API}/auth/me', 405), ('GET', f'{API}/claims/%2e%2e%2f%2e%2e', 404)):
                r = c.request(method, path)
                self.assertEqual(r.status_code, status, (method, path))
                self.assertEqual(set(r.json()), {'error'})
                self.assertNotIn('Traceback', r.text)


class ClaimsApiTests(unittest.TestCase):
    def setUp(self):
        self.w = ApiWorld()
        self.w.provision_all()
        self.claim_id, self.high_rule = self.w.high()
        self.raw_claim, _ = self.w.claims.get(self.claim_id)
        self.raw_ids = public_raw_ids(self.raw_claim)

    def tearDown(self):
        self.w.close()

    def detail(self, level, claim_id=None):
        return self.w.login_client(level).get(f'{API}/claims/{claim_id or self.claim_id}')

    def test_no_level_ever_receives_a_raw_identifier_from_the_detail_endpoint(self):
        self.assertTrue(self.raw_ids)
        for level in (1, 2, 3):
            r = self.detail(level)
            self.assertEqual(r.status_code, 200)
            for raw in self.raw_ids:
                self.assertNotIn(raw, r.text, f'level {level}')

    def test_the_viewer_has_no_notes_and_the_reviewer_has_them(self):
        v, rv = self.detail(1).json(), self.detail(2).json()
        self.assertNotIn('notes', v['claim'])
        self.assertTrue(all('text' not in a for a in v['claim']['attachments']))
        self.assertIn('notes', rv['claim'])

    def test_allowed_actions_are_computed_for_the_caller_and_absent_when_not_allowed(self):
        for level, expect_high_actions in ((1, False), (2, False), (3, True)):
            body = self.detail(level).json()
            finding = next(f for f in body['findings'] if f['rule_id'] == self.high_rule)
            self.assertEqual('allowed_actions' in finding, expect_high_actions, level)
        self.assertNotIn('allowed_actions', self.detail(1).json())
        self.assertIn('unmask', self.detail(2).json()['allowed_actions'])

    def test_unknown_and_malformed_claim_ids_give_the_same_404(self):
        c = self.w.login_client(2)
        bodies = {c.get(f'{API}/claims/{x}').content for x in ('CG-NOPE', 'x' * 300, '%00', 'CG-1;DROP', 'cg-lower')}
        self.assertEqual(len(bodies), 1)
        self.assertEqual(json.loads(bodies.pop()), {'error': 'not_found'})

    def test_list_parameters_are_validated(self):
        c = self.w.login_client(1)
        self.assertEqual(c.get(f'{API}/claims', params={'limit': 5}).status_code, 200)
        self.assertEqual(len(c.get(f'{API}/claims', params={'limit': 5}).json()['claims']), 5)
        for params in ({'limit': 0}, {'limit': 201}, {'limit': 'x'}, {'offset': -1}, {'status': 'MAYBE'}, {'rule_id': 'R1'}, {'rule_id': "R001' OR 1=1"}):
            self.assertEqual(c.get(f'{API}/claims', params=params).status_code, 422, params)
        listing = c.get(f'{API}/claims', params={'limit': 200}).text
        for raw in self.raw_ids:
            self.assertNotIn(raw, listing)

    def test_unmask_needs_a_reason_returns_real_identifiers_for_one_claim_and_is_audited(self):
        c = self.w.login_client(2)
        self.assertEqual(c.post(f'{API}/claims/{self.claim_id}/unmask', json={}).status_code, 422)
        self.assertEqual(c.post(f'{API}/claims/{self.claim_id}/unmask', json={'reason': ''}).status_code, 422)
        r = c.post(f'{API}/claims/{self.claim_id}/unmask', json={'reason': 'Calling the member back'})
        self.assertEqual(r.status_code, 200)
        for raw in self.raw_ids:
            self.assertIn(raw, r.text)
        self.assertIn('"unmask"', self.w.log_text().replace(' ', ''))
        other = next(row['claim_id'] for row in self.w.claims.summaries(limit=200) if row['claim_id'] != self.claim_id)
        self.assertNotIn(next(iter(self.raw_ids)), c.get(f'{API}/claims/{other}').text)      # unmasking one claim unmasks no other

    def test_a_redaction_failure_returns_an_error_not_the_data(self):
        c = self.w.login_client(2)
        with mock.patch.object(masking, 'scrub', lambda obj, mapping: obj):
            r = c.get(f'{API}/claims/{self.claim_id}')
        self.assertEqual(r.status_code, 500)
        self.assertEqual(r.json(), {'error': 'server_error'})
        for raw in self.raw_ids:
            self.assertNotIn(raw, r.text)


class DecisionApiTests(unittest.TestCase):
    def setUp(self):
        self.w = ApiWorld()
        self.w.provision_all()
        self.med_claim, self.med_rule = self.w.medium()
        self.high_claim, self.high_rule = self.w.high()

    def tearDown(self):
        self.w.close()

    def post(self, c, claim, rule, body):
        return c.post(f'{API}/claims/{claim}/findings/{rule}/decision', json=body)

    def review_events(self):
        path = self.w.review_log.path
        if not path.exists():
            return []
        return [json.loads(l)['event'] for l in path.read_text(encoding='utf-8').split('\n') if l.strip()]

    def test_the_reviewer_recorded_is_the_logged_in_badge(self):
        c = self.w.login_client(3)
        r = self.post(c, self.high_claim, self.high_rule, {'action': 'confirm_issue', 'reason': 'Checked the evidence'})
        self.assertEqual(r.status_code, 200, r.text)
        event = [e for e in self.review_events() if e.get('claim_id') == self.high_claim][-1]
        self.assertEqual((event['actor'], event['action'], event['rule_id']), (BADGES[3], 'confirm_issue', self.high_rule))

    def test_the_client_cannot_choose_who_decided_or_what_it_saw(self):
        c = self.w.login_client(2)
        for extra in ({'actor': 'CG-3003'}, {'original_status': 'FAIL'}, {'created_at': '2020-01-01T00:00:00Z'}, {'claim_id': 'x'}, {'rule_id': 'R001'}):
            r = self.post(c, self.med_claim, self.med_rule, {'action': 'confirm_issue', 'reason': 'ok ok', **extra})
            self.assertEqual(r.status_code, 422, extra)
        self.assertEqual([e for e in self.review_events() if e.get('actor') == 'CG-3003'], [])

    def test_a_reviewer_cannot_decide_a_high_severity_finding_and_the_refusal_is_audited(self):
        c = self.w.login_client(2)
        r = self.post(c, self.high_claim, self.high_rule, {'action': 'confirm_issue', 'reason': 'Checked'})
        self.assertEqual((r.status_code, r.json()), (403, {'error': 'forbidden'}))
        self.assertEqual([e for e in self.review_events() if e.get('claim_id') == self.high_claim], [])
        self.assertIn('"forbidden"', self.w.log_text().replace(' ', ''))

    def test_invalid_decisions_are_422(self):
        c = self.w.login_client(2)
        bad = [{'action': 'approve', 'reason': 'x'}, {'action': 'confirm_issue'}, {'action': 'confirm_issue', 'reason': ''},
               {'action': 'confirm_issue', 'reason': 'x' * 1001}, {'action': ['confirm_issue'], 'reason': 'x'}, {'action': 'confirm_issue', 'reason': 5}]
        for body in bad:
            self.assertEqual(self.post(c, self.med_claim, self.med_rule, body).status_code, 422, body)
        self.assertEqual(self.review_events(), [])

    def test_a_finding_that_passed_cannot_be_reviewed(self):
        c = self.w.login_client(3)
        for row in self.w.claims.summaries(limit=200):
            _, results = self.w.claims.get(row['claim_id'])
            passed = next((r for r in results if r['status'] == 'PASS'), None)
            if passed:
                r = self.post(c, row['claim_id'], passed['rule_id'], {'action': 'confirm_issue', 'reason': 'whatever'})
                self.assertEqual(r.status_code, 422)
                self.assertEqual(r.json()['error'], 'decision_rejected')
                return
        self.fail('no passing finding in the data')

    def test_unknown_claims_and_rules_are_404(self):
        c = self.w.login_client(3)
        self.assertEqual(self.post(c, 'CG-NOPE', 'R001', {'action': 'confirm_issue', 'reason': 'x'}).status_code, 404)
        self.assertEqual(self.post(c, self.med_claim, 'R999', {'action': 'confirm_issue', 'reason': 'x'}).status_code, 404)

    def test_the_review_state_follows_the_decision(self):
        c = self.w.login_client(2)
        def state():
            f = next(f for f in c.get(f'{API}/claims/{self.med_claim}').json()['findings'] if f['rule_id'] == self.med_rule)
            return f.get('review_state')
        self.assertEqual(state(), 'unreviewed')
        self.assertEqual(self.post(c, self.med_claim, self.med_rule, {'action': 'confirm_issue', 'reason': 'Checked'}).status_code, 200)
        self.assertEqual(state(), 'resolved')

    def test_the_decision_and_the_security_log_both_verify_afterwards(self):
        c = self.w.login_client(3)
        self.post(c, self.high_claim, self.high_rule, {'action': 'confirm_issue', 'reason': 'Checked the evidence'})
        self.assertIn('"decision"', self.w.log_text().replace(' ', ''))
        admin = self.w.login_client(4)
        v = admin.get(f'{API}/audit/verify').json()
        self.assertTrue(v['security']['ok'], v)
        self.assertTrue(v['review']['ok'], v)


class AuditApiTests(unittest.TestCase):
    def test_the_audit_endpoints_show_events_and_verification(self):
        with ApiWorld() as w:
            w.provision_all()
            w.login_client(2)
            admin = w.login_client(4)
            events = admin.get(f'{API}/audit/events', params={'limit': 500}).json()['events']
            self.assertTrue(any(e['event']['event_type'] == 'login_success' for e in events))
            self.assertEqual(admin.get(f'{API}/audit/events', params={'limit': 0}).status_code, 422)
            self.assertEqual(admin.get(f'{API}/audit/events', params={'offset': -1}).status_code, 422)
            text = json.dumps(events)
            for secret in list(PASSWORDS.values()) + list(w.secrets.values()):
                self.assertNotIn(secret, text)
            self.assertTrue(admin.get(f'{API}/audit/verify').json()['security']['ok'])


class UserApiTests(unittest.TestCase):
    def setUp(self):
        self.w = ApiWorld()
        self.w.provision_all()
        self.admin = self.w.login_client(4)

    def tearDown(self):
        self.w.close()

    def test_creating_a_user_returns_the_authenticator_uri_once_and_never_the_hash(self):
        r = self.admin.post(f'{API}/users', json={'badge_id': 'CG-5005', 'name': 'New Person', 'password': 'Fresh-Pass-8642!', 'level': 2})
        self.assertEqual(r.status_code, 201)
        self.assertEqual(set(r.json()), {'badge_id', 'provisioning_uri'})
        self.assertIn('otpauth://totp/', r.json()['provisioning_uri'])
        listing = self.admin.get(f'{API}/users').text
        for forbidden in ('password_hash', 'totp_secret', 'otpauth', '$2b$'):
            self.assertNotIn(forbidden, listing)
        row = next(u for u in self.admin.get(f'{API}/users').json()['users'] if u['badge_id'] == 'CG-5005')
        self.assertEqual(set(row), {'badge_id', 'name', 'level', 'active', 'locked', 'must_change_password', 'last_login'})

    def test_bad_user_requests(self):
        for body in ({'badge_id': 'CG-5005', 'name': 'N', 'password': 'short', 'level': 2},
                     {'badge_id': 'bad', 'name': 'N', 'password': 'Fresh-Pass-8642!', 'level': 2},
                     {'badge_id': 'CG-5005', 'name': 'N', 'password': 'Fresh-Pass-8642!', 'level': 9},
                     {'badge_id': 'CG-5005', 'name': 'N', 'password': 'Fresh-Pass-8642!', 'level': '2'},
                     {'badge_id': 'CG-5005', 'name': 'N', 'password': 'Fresh-Pass-8642!', 'level': 2, 'is_admin': True},
                     {'badge_id': BADGES[2], 'name': 'N', 'password': 'Fresh-Pass-8642!', 'level': 2}):
            self.assertIn(self.admin.post(f'{API}/users', json=body).status_code, (409, 422), body)

    def test_mass_assignment_and_escalation_through_the_api(self):
        for field, value in (('password_hash', 'x'), ('failed_attempts', 0), ('totp_secret_enc', 'x'), ('badge_id', 'CG-9'), ('role', 'admin')):
            self.assertEqual(self.admin.patch(f'{API}/users/{BADGES[2]}', json={field: value}).status_code, 422, field)
        self.assertEqual(self.admin.patch(f'{API}/users/{BADGES[2]}', json={'level': 5}).status_code, 422)
        self.assertEqual(self.admin.patch(f'{API}/users/{BADGES[4]}', json={'level': 3}).status_code, 403)          # own level
        self.assertEqual(self.admin.patch(f'{API}/users/{BADGES[2]}', json={'grants': ['users.manage']}).status_code, 403)
        self.assertEqual(self.admin.patch(f'{API}/users/CG-9999', json={'name': 'x'}).status_code, 404)
        self.assertEqual(self.w.store.get_user(BADGES[2]).level, 2)

    def test_a_demotion_takes_effect_on_the_users_live_session(self):
        reviewer = self.w.login_client(2)
        self.assertEqual(reviewer.get(f'{API}/claims').status_code, 200)
        self.assertEqual(self.admin.patch(f'{API}/users/{BADGES[2]}', json={'level': 4}).status_code, 200)
        self.assertEqual(reviewer.get(f'{API}/claims').status_code, 403)                  # level 4 does not read claims
        self.assertEqual(reviewer.get(f'{API}/users').status_code, 200)


class HardeningTests(unittest.TestCase):
    def setUp(self):
        self.w = ApiWorld()
        self.w.provision_all()

    def tearDown(self):
        self.w.close()

    def test_security_headers_are_on_every_kind_of_response(self):
        c = self.w.login_client(2)
        responses = [self.w.client().get('/healthz'), c.get(f'{API}/auth/me'), self.w.client().get(f'{API}/auth/me'),
                     c.get(f'{API}/users'), c.get(f'{API}/claims/CG-NOPE'), c.get(f'{API}/claims', params={'limit': 0})]
        self.assertEqual(sorted({r.status_code for r in responses}), [200, 401, 403, 404, 422])
        for r in responses:
            h = r.headers
            self.assertEqual(h['cache-control'], 'no-store')
            self.assertEqual(h['x-content-type-options'], 'nosniff')
            self.assertEqual(h['x-frame-options'], 'DENY')
            self.assertEqual(h['referrer-policy'], 'no-referrer')
            self.assertEqual(h['content-security-policy'], "default-src 'none'")

    def test_verify_treats_a_review_log_with_no_decisions_yet_as_intact(self):
        c = self.w.login_client(4)
        self.assertFalse(self.w.review_log.path.exists())
        body = c.get(f'{API}/audit/verify').json()
        self.assertEqual(body['review'], {'ok': True, 'events': 0})

    def test_oversized_bodies_are_refused(self):
        c = self.w.client()
        big = {'badge_id': 'CG-2002', 'password': 'x' * 70_000, 'totp': '123456'}
        self.assertEqual(c.post(f'{API}/auth/login', json=big).status_code, 413)
        def chunks():
            for _ in range(80):
                yield b'x' * 1024
        self.assertEqual(c.post(f'{API}/auth/login', content=chunks(), headers={'content-type': 'application/json'}).status_code, 413)

    def test_a_body_must_be_json(self):
        c = self.w.client()
        r = c.post(f'{API}/auth/login', content='badge_id=CG-2002', headers={'content-type': 'application/x-www-form-urlencoded'})
        self.assertEqual(r.status_code, 415)
        r = c.post(f'{API}/auth/login', content='{"badge_id"', headers={'content-type': 'application/json'})
        self.assertEqual(r.status_code, 422)

    def test_health_reveals_nothing_and_needs_no_session(self):
        r = self.w.client().get('/healthz')
        self.assertEqual((r.status_code, r.json()), (200, {'status': 'ok'}))

    def test_an_unexpected_error_is_a_plain_500_with_no_details(self):
        c = self.w.login_client(2)
        with mock.patch.object(self.w.claims, 'summaries', side_effect=RuntimeError('db password is hunter2')):
            r = c.get(f'{API}/claims')
        self.assertEqual((r.status_code, r.json()), (500, {'error': 'server_error'}))
        self.assertNotIn('hunter2', r.text)

    def test_the_store_going_down_is_a_503_and_never_a_success(self):
        w = self.w
        w.provision(2, badge='CG-2666')
        c = w.client()
        with mock.patch.object(w.store, 'get_user', side_effect=__import__('access.store', fromlist=['x']).StoreUnavailable('down')):
            r = c.post(f'{API}/auth/login', json={'badge_id': 'CG-2666', 'password': PASSWORDS[2], 'totp': '123456'})
        self.assertEqual((r.status_code, r.json()), (503, {'error': 'unavailable'}))


if __name__ == '__main__':
    unittest.main()
