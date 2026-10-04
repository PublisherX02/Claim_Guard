"""Attacks against the reviewer API, written the way an attacker would try them. Each test states the attack and what must hold.

Where a real MongoDB is available (MONGO_URI), the injection and token attacks also run against it, because injection is a
property of the database driver as much as of our code.
"""
import base64
import json
import os
import statistics
import sys
import time
import unittest
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import jwt
from access import store, tokens
from access_api_world import ApiWorld
from access_world import BADGES, PASSWORDS

API = '/api/v1'
MONGO_URI = os.environ.get('MONGO_URI')
LEAK_WORDS = ('Traceback', 'File "', 'pydantic', 'starlette', 'fastapi', 'pymongo', 'bson', 'uvicorn', '.py"', 'site-packages')


def asgi_request(app, method, raw_path, headers=(), body=b''):
    """Send a request straight to the application with exactly these bytes, so the test client's own validation cannot hide a case."""
    import asyncio
    path, _, query = raw_path.partition(b'?')
    scope = {'type': 'http', 'asgi': {'version': '3.0'}, 'http_version': '1.1', 'method': method, 'scheme': 'http', 'raw_path': path,
             'path': path.decode('latin-1'), 'query_string': query, 'root_path': '', 'client': ('127.0.0.1', 5555), 'server': ('testserver', 80),
             'headers': [(b'host', b'testserver'), *headers]}
    sent = []
    state = {'delivered': False}

    async def receive():
        if not state['delivered']:
            state['delivered'] = True
            return {'type': 'http.request', 'body': body, 'more_body': False}
        return {'type': 'http.disconnect'}

    async def send(message):
        sent.append(message)
    asyncio.run(app(scope, receive, send))
    status = next(m['status'] for m in sent if m['type'] == 'http.response.start')
    return status, b''.join(m.get('body', b'') for m in sent if m['type'] == 'http.response.body')


def mongo_world(**kw):
    from access.store_mongo import MongoStore
    st = MongoStore(MONGO_URI, 'claimguard_test_' + uuid.uuid4().hex[:12])
    st.ensure_indexes()
    w = ApiWorld(store_obj=st, **kw)
    w._mongo = st
    return w


class AttackBase:
    """Subclasses define new_world(). Every test builds its own world so no attack can influence another."""

    def new_world(self, **kw):
        raise NotImplementedError

    def setUp(self):
        self.w = self.new_world()
        self.w.provision_all()

    def tearDown(self):
        mongo = getattr(self.w, '_mongo', None)
        self.w.close()
        if mongo:
            mongo.drop_database()
            mongo.close()

    # ---- credentials -------------------------------------------------------------------------------------------
    def login(self, client, badge, password, code):
        return client.post(f'{API}/auth/login', json={'badge_id': badge, 'password': password, 'totp': code})

    def test_brute_force_one_account_locks_it_and_it_stays_locked(self):
        w = self.w
        statuses = set()
        for i in range(100):
            w.clock.advance(1)
            c = w.client()
            statuses.add(self.login(c, BADGES[2], f'Guess-{i:04d}-Pass!', '000000').status_code)
        self.assertLessEqual(statuses, {401, 429})
        self.assertIsNotNone(w.store.get_user(BADGES[2]).locked_until)
        w.clock.advance(1)
        self.assertNotEqual(self.login(w.client(), BADGES[2], PASSWORDS[2], w.code(BADGES[2])).status_code, 200)

    def test_credential_stuffing_across_many_badges_is_stopped_by_the_address_throttle(self):
        c = self.w.client()
        codes = [self.login(c, f'CG-{9000 + i}', 'Leaked-Pass-1234!', '123456').status_code for i in range(40)]
        self.assertEqual(codes[:20], [401] * 20)
        self.assertEqual(set(codes[20:]), {429})

    def test_a_spoofed_forwarded_for_header_does_not_buy_a_fresh_throttle_bucket(self):
        c = self.w.client()
        codes = [c.post(f'{API}/auth/login', json={'badge_id': 'CG-9000', 'password': 'Leaked-Pass-1234!', 'totp': '123456'},
                        headers={'X-Forwarded-For': f'10.0.0.{i}', 'X-Real-IP': f'10.1.0.{i}'}).status_code for i in range(25)]
        self.assertEqual(codes[-1], 429)

    def test_a_stolen_password_and_a_replayed_code_do_not_log_in(self):
        w = self.w
        code = w.code(BADGES[2])
        self.assertEqual(self.login(w.client(), BADGES[2], PASSWORDS[2], code).status_code, 200)
        self.assertEqual(self.login(w.client(), BADGES[2], PASSWORDS[2], code).status_code, 401)
        self.assertEqual(self.login(w.client(), BADGES[2], PASSWORDS[2], '000000').status_code, 401)

    def test_enumeration_by_response_and_by_time(self):
        with ApiWorld(BCRYPT_ROUNDS=10) as w:
            w.provision(1)
            w.provision(2, badge='CG-2999')
            w.store.update_user('CG-2999', active=False)

            def timed(badge, password):
                samples = []
                for _ in range(9):
                    c = w.client()
                    t = time.perf_counter()
                    r = c.post(f'{API}/auth/login', json={'badge_id': badge, 'password': password, 'totp': '123456'})
                    samples.append(time.perf_counter() - t)
                    self.assertEqual((r.status_code, r.content), (401, b'{"error":"invalid_credentials"}'))
                    w.clock.advance(1)
                w.clock.advance(901)                             # let the address throttle forget these attempts
                return statistics.median(samples)
            wrong = timed(BADGES[1], 'Wrong-Pass-1357!')
            for label, value in (('unknown', timed('CG-7777', 'Wrong-Pass-1357!')), ('inactive', timed('CG-2999', PASSWORDS[2]))):
                self.assertTrue(0.6 < value / wrong < 1.7, f'{label}: {value / wrong:.2f}')

    # ---- tokens and sessions -----------------------------------------------------------------------------------
    def with_cookie(self, token):
        c = self.w.client()
        c.cookies.set('cg_session', token)
        return c

    def test_forged_tampered_and_downgraded_tokens_are_refused(self):
        w = self.w
        good = w.login_client(2).cookies.get('cg_session')
        head, payload, sig = good.split('.')
        flipped = payload[:-2] + ('AA' if payload[-2:] != 'AA' else 'BB')

        def b64(o):
            return base64.urlsafe_b64encode(json.dumps(o).encode()).rstrip(b'=').decode()
        claims = {'sub': BADGES[4], 'jti': 'j', 'csrf': 'c', 'iat': int(w.clock.now), 'exp': int(w.clock.now) + 600}
        forged = {
            'tampered payload': f'{head}.{flipped}.{sig}',
            'wrong secret': jwt.encode(claims, 'x' * 40, algorithm='HS256'),
            'alg none': b64({'alg': 'none', 'typ': 'JWT'}) + '.' + b64(claims) + '.',
            'HS512 with the right secret': jwt.encode(claims, w.settings.jwt_secret, algorithm='HS512'),
            'expired': jwt.encode({**claims, 'exp': int(w.clock.now) - 5}, w.settings.jwt_secret, algorithm='HS256'),
            'not yet valid': jwt.encode({**claims, 'iat': int(w.clock.now) + 999, 'exp': int(w.clock.now) + 9999}, w.settings.jwt_secret, algorithm='HS256'),
            'empty': '', 'garbage': 'not.a.token', 'huge': 'A' * 100_000,
        }
        for label, token in forged.items():
            self.assertEqual(self.with_cookie(token).get(f'{API}/auth/me').status_code, 401, label)

    def test_a_correctly_signed_token_for_a_user_who_does_not_exist_or_is_gone_is_refused(self):
        w = self.w
        ghost = tokens.issue(w.settings, 'CG-9999', now=w.clock.now).token
        self.assertEqual(self.with_cookie(ghost).get(f'{API}/auth/me').status_code, 401)
        c = w.login_client(2)
        w.store.update_user(BADGES[2], active=False)
        self.assertEqual(c.get(f'{API}/auth/me').status_code, 401)

    def test_forged_privilege_claims_inside_a_valid_signature_change_nothing(self):
        w = self.w
        good = w.login_client(1).cookies.get('cg_session')
        payload = jwt.decode(good, w.settings.jwt_secret, algorithms=['HS256'], options={'verify_exp': False, 'verify_iat': False})
        payload.update({'level': 4, 'role': 'admin', 'permissions': ['users.manage', 'claims.decide_high']})
        forged = jwt.encode(payload, w.settings.jwt_secret, algorithm='HS256')
        c = self.with_cookie(forged)
        me = c.get(f'{API}/auth/me').json()
        self.assertEqual(me['level'], 1)
        self.assertEqual(c.get(f'{API}/users').status_code, 403)

    def test_a_session_ends_on_logout_demotion_and_password_change(self):
        w = self.w
        a = w.login_client(2)
        a.post(f'{API}/auth/logout')
        old = a.cookies.get('cg_session')
        self.assertEqual(self.with_cookie(old).get(f'{API}/auth/me').status_code, 401)
        b = w.login_client(3)
        w.store.update_user(BADGES[3], level=1)
        self.assertEqual(b.get(f'{API}/users').status_code, 403)
        self.assertEqual(b.get(f'{API}/claims').status_code, 200)
        c = w.login_client(1)
        r = c.post(f'{API}/auth/change-password', json={'old_password': PASSWORDS[1], 'new_password': 'Brand-New-Pass-2468!'})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(c.get(f'{API}/auth/me').status_code, 401)

    def test_logging_in_always_issues_a_new_token_and_a_planted_one_is_worthless(self):
        w = self.w
        planted = 'attacker-chosen-session-value'
        w.clock.advance(31)
        client = w.client()
        client.cookies.set('cg_session', planted, domain='testserver')
        r = self.login(client, BADGES[2], PASSWORDS[2], w.code(BADGES[2]))
        self.assertEqual(r.status_code, 200)
        issued = next(h for h in r.headers.get_list('set-cookie') if h.startswith('cg_session=')).split(';')[0].split('=', 1)[1]
        self.assertNotEqual(issued, planted)
        self.assertEqual(self.with_cookie(issued).get(f'{API}/auth/me').json()['badge_id'], BADGES[2])
        self.assertEqual(self.with_cookie(planted).get(f'{API}/auth/me').status_code, 401)
        w.clock.advance(31)
        again = self.login(w.client(), BADGES[2], PASSWORDS[2], w.code(BADGES[2]))
        second = next(h for h in again.headers.get_list('set-cookie') if h.startswith('cg_session=')).split(';')[0].split('=', 1)[1]
        self.assertNotEqual(second, issued)

    # ---- injection ----------------------------------------------------------------------------------------------
    def test_operator_injection_in_every_field_never_logs_in_never_crashes_and_never_matches(self):
        w = self.w
        evil = [{'$ne': None}, {'$gt': ''}, {'$regex': '.*'}, {'$where': '1==1'}, ['CG-2002'], [{'$ne': 1}], {'$or': [{}]}, '{"$ne": null}']
        c = w.login_client(2)
        admin = w.login_client(4)
        fields = ('badge_id', 'password', 'totp')
        for bad in evil:
            for field in fields:
                body = {'badge_id': BADGES[2], 'password': PASSWORDS[2], 'totp': '123456', field: bad}
                r = w.client().post(f'{API}/auth/login', json=body)
                self.assertIn(r.status_code, (401, 422, 429), (field, bad))
        for bad in evil:
            path_bad = json.dumps(bad)
            for method, path, kwargs in (('POST', f'{API}/claims/{path_bad}/unmask', {'json': {'reason': 'x'}}),
                                         ('GET', f'{API}/claims/{path_bad}', {}),
                                         ('POST', f'{API}/claims/x/findings/{path_bad}/decision', {'json': {'action': 'confirm_issue', 'reason': 'x'}}),
                                         ('POST', f'{API}/claims/x/unmask', {'json': {'reason': bad}}),
                                         ('POST', f'{API}/claims/x/findings/R001/decision', {'json': {'action': bad, 'reason': 'x'}}),
                                         ('GET', f'{API}/claims', {'params': {'status': path_bad, 'rule_id': path_bad}})):
                r = c.request(method, path, **kwargs)
                self.assertLess(r.status_code, 500, (method, path, bad))
                self.assertNotEqual(r.status_code, 200 if method == 'POST' else 0)
        for bad in evil:
            for path in (f'{API}/users/{json.dumps(bad)}', f'{API}/users/{json.dumps(bad)}/unlock', f'{API}/users/{json.dumps(bad)}/reset-totp'):
                r = admin.request('PATCH' if path.endswith(json.dumps(bad)) else 'POST', path, json={'name': 'x'} if 'unlock' not in path and 'reset' not in path else None)
                self.assertIn(r.status_code, (404, 405, 422), (path, r.status_code))
            r = admin.post(f'{API}/users', json={'badge_id': bad, 'name': 'x', 'password': 'Fresh-Pass-8642!', 'level': 2})
            self.assertEqual(r.status_code, 422)
        self.assertEqual(len(w.store.list_users()), 4)

    def test_mass_assignment_and_privilege_escalation_by_an_administrator(self):
        w = self.w
        admin = w.login_client(4)
        for body in ({'level': 5}, {'level': 4.5}, {'level': '4'}, {'level': True}, {'grants': ['claims.decide_high', 'users.manage']},
                     {'grants': 'users.manage'}, {'password_hash': 'x'}, {'failed_attempts': 0}, {'locked_until': None}, {'__proto__': {'level': 4}}):
            r = admin.patch(f'{API}/users/{BADGES[2]}', json=body)
            self.assertIn(r.status_code, (403, 422), body)
        self.assertEqual(admin.post(f'{API}/users', json={'badge_id': 'CG-5005', 'name': 'x', 'password': 'Fresh-Pass-8642!', 'level': 5}).status_code, 422)
        self.assertEqual(admin.patch(f'{API}/users/{BADGES[4]}', json={'level': 3}).status_code, 403)
        self.assertEqual(admin.patch(f'{API}/users/{BADGES[4]}', json={'active': False}).status_code, 403)
        self.assertEqual(w.store.get_user(BADGES[2]).level, 2)
        self.assertEqual(w.store.get_user(BADGES[2]).grants, ())

    def test_a_non_admin_cannot_manage_users_by_any_route(self):
        for level in (1, 2, 3):
            c = self.w.login_client(level)
            for method, path, kw in (('GET', f'{API}/users', {}), ('POST', f'{API}/users', {'json': {'badge_id': 'CG-5005', 'name': 'x', 'password': 'Fresh-Pass-8642!', 'level': 4}}),
                                     ('PATCH', f'{API}/users/{BADGES[level]}', {'json': {'level': 4}}), ('POST', f'{API}/users/{BADGES[1]}/reset-totp', {})):
                self.assertEqual(c.request(method, path, **kw).status_code, 403, (level, method, path))
        self.assertEqual(self.w.store.get_user(BADGES[2]).level, 2)

    # ---- transport-level abuse ---------------------------------------------------------------------------------
    def test_no_malformed_request_produces_a_server_error_or_leaks_internals(self):
        c = self.w.login_client(2)
        deep = '[' * 5000 + ']' * 5000
        nasty = [
            ('POST', f'{API}/auth/login', {'content': b'\xff\xfe\x00\x01', 'headers': {'content-type': 'application/json'}}),
            ('POST', f'{API}/auth/login', {'content': deep, 'headers': {'content-type': 'application/json'}}),
            ('POST', f'{API}/auth/login', {'content': '{"badge_id":' + '1' * 5000 + '}', 'headers': {'content-type': 'application/json'}}),
            ('POST', f'{API}/auth/login', {'content': '\x00\x00', 'headers': {'content-type': 'application/json; charset=utf-16'}}),
            ('GET', f'{API}/claims', {'params': {'limit': '9' * 400}}),
            ('GET', f'{API}/claims', {'params': {'status': 'A' * 100_000}}),
            ('GET', f'{API}/claims/' + 'A' * 100_000, {}),
            ('GET', f'{API}/audit/events', {'headers': {'X-CSRF-Token': 'a\r\nSet-Cookie: x=1'}}),
            ('GET', '/' + '%00' * 50, {}), ('GET', '/%', {}), ('GET', '//' * 100, {}),
            ('POST', f'{API}/claims/x/unmask', {'json': {'reason': 'r' * 70_000}}),
            ('TRACE', f'{API}/claims', {}), ('PATCH', f'{API}/claims', {}),
        ]
        for method, path, kw in nasty:
            try:
                r = c.request(method, path, **kw)
            except Exception as e:                          # noqa: BLE001 -- the client library refused to even send it
                if type(e).__module__.split('.')[0] in ('httpx', 'httpx2', 'httpcore', 'httpcore2', 'h11'):
                    continue
                raise
            self.assertLess(r.status_code, 500, (method, path[:40], r.status_code))
            for word in LEAK_WORDS:
                self.assertNotIn(word, r.text, (method, path[:40], word))

    def test_raw_hostile_requests_sent_straight_to_the_application(self):
        w = self.w
        session = w.login_client(2).cookies.get('cg_session')
        cookie = (b'cookie', b'cg_session=' + session.encode())
        json_type = (b'content-type', b'application/json')
        cases = [
            ('GET', b'/%', ()), ('GET', b'/%00%00%00', ()), ('GET', b'//' * 100, ()), ('GET', b'/api/v1/claims/%ff%fe', (cookie,)),
            ('GET', b'/api/v1/claims?limit=%ff', (cookie,)), ('GET', b'/api/v1/claims?' + b'a=1&' * 5000, (cookie,)),
            ('POST', b'/api/v1/auth/login', ((b'content-length', b'abc'), json_type)),
            ('POST', b'/api/v1/auth/login', ((b'content-length', b'-5'), json_type)),
            ('POST', b'/api/v1/auth/login', ((b'content-length', b'99999999999999999999'), json_type)),
            ('GET', b'/api/v1/audit/events', (cookie, (b'x-csrf-token', b'a\r\nSet-Cookie: x=1'))),
            ('POST', b'/api/v1/auth/logout', (cookie, (b'x-csrf-token', b'\xff\xfe\x00'))),
            ('GET', b'/api/v1/auth/me', ((b'cookie', b'cg_session=' + b'A' * 70000),)),
            ('GET', b'/api/v1/auth/me', ((b'cookie', b'cg_session=\xff\xfe'),)),
            ('GET', b'/api/v1/auth/me', ((b'authorization', b'Bearer ' + session.encode()),)),
        ]
        for method, raw, headers in cases:
            status, body = asgi_request(self.w.app, method, raw, headers)
            self.assertLess(status, 500, (method, raw[:40], status))
            text = body.decode('latin-1')
            for word in LEAK_WORDS:
                self.assertNotIn(word, text, (method, raw[:40], word))
        status, _ = asgi_request(self.w.app, 'GET', b'/api/v1/auth/me', ((b'authorization', b'Bearer ' + session.encode()),))
        self.assertEqual(status, 401)                                  # a bearer token is not a session: only the cookie is

    def test_duplicate_cookies_and_method_override_headers_change_nothing(self):
        w = self.w
        c = w.login_client(2)
        ok = c.get(f'{API}/claims', params={'limit': 1}).status_code
        r = c.get(f'{API}/claims', params={'limit': 1}, headers={'Cookie': 'cg_session=junk; cg_session=junk2'})
        self.assertIn(r.status_code, (200, 401))
        before = c.post(f'{API}/auth/logout').status_code if False else None
        override = c.get(f'{API}/claims', params={'limit': 1}, headers={'X-HTTP-Method-Override': 'DELETE', 'X-Method-Override': 'DELETE'})
        self.assertEqual(override.status_code, ok)
        self.assertEqual(c.get(f'{API}/claims', params={'limit': 1}).status_code, ok)

    def test_no_cross_origin_access_is_granted(self):
        c = self.w.login_client(2)
        for r in (c.get(f'{API}/auth/me', headers={'Origin': 'https://evil.example'}),
                  c.options(f'{API}/claims', headers={'Origin': 'https://evil.example', 'Access-Control-Request-Method': 'GET'})):
            self.assertFalse(any(h.lower().startswith('access-control-') for h in r.headers), r.headers)

    def test_cookies_are_scoped_to_the_server_only(self):
        r = self.w.client().post(f'{API}/auth/login', json={'badge_id': BADGES[2], 'password': PASSWORDS[2], 'totp': self.w.code(BADGES[2])})
        for header in r.headers.get_list('set-cookie'):
            self.assertNotIn('domain=', header.lower())
            self.assertIn('path=/', header.lower())

    def test_an_error_never_contains_the_secrets_it_was_triggered_with(self):
        c = self.w.client()
        r = c.post(f'{API}/auth/login', json={'badge_id': BADGES[2], 'password': 'Planted-Secret-Pass-9!', 'totp': {'x': 'Planted-Secret-Pass-9!'}})
        self.assertNotIn('Planted-Secret-Pass-9', r.text)

    # ---- data exposure -----------------------------------------------------------------------------------------
    def test_no_endpoint_ever_returns_a_hash_a_seed_or_a_key(self):
        w = self.w
        admin = w.login_client(4)
        seen = [admin.get(f'{API}/users').text, admin.get(f'{API}/audit/events', params={'limit': 1000}).text, admin.get(f'{API}/auth/me').text]
        creation = admin.post(f'{API}/users', json={'badge_id': 'CG-5005', 'name': 'x', 'password': 'Fresh-Pass-8642!', 'level': 2}).text
        seen.append(creation.replace(creation[creation.find('secret='):creation.find('&issuer')], ''))   # the one-time URI is the only allowed carrier
        blob = ' '.join(seen)
        for forbidden in ('$2b$', 'password_hash', 'totp_secret', 'gAAAA', w.settings.jwt_secret, w.settings.fernet_key, w.settings.pii_key,
                          w.settings.audit_anchor_key, *w.secrets.values()):
            self.assertNotIn(forbidden, blob, forbidden[:12])


class MemoryAttacks(AttackBase, unittest.TestCase):
    def new_world(self, **kw):
        return ApiWorld(**kw)


@unittest.skipUnless(MONGO_URI, 'MONGO_URI not set: the attack suite was NOT run against MongoDB')
class MongoAttacks(AttackBase, unittest.TestCase):
    def new_world(self, **kw):
        return mongo_world(**kw)


class AttackSuiteIsConfigured(unittest.TestCase):
    def test_mongo_attacks_are_required_in_ci(self):
        if os.environ.get('REQUIRE_MONGO') == '1':
            self.assertTrue(MONGO_URI, 'REQUIRE_MONGO=1 but MONGO_URI is not set')
        elif not MONGO_URI:
            self.skipTest('MONGO_URI not set: the attack suite was NOT run against MongoDB')


if __name__ == '__main__':
    unittest.main()
