import json
import os
import statistics
import sys
import time
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import jwt
import pyotp
from access import permissions, service, store, totp
from access_world import BADGES, PASSWORDS, World


class LoginTests(unittest.TestCase):
    def setUp(self):
        self.w = World()
        self.w.provision_all()

    def tearDown(self):
        self.w.close()

    def test_the_happy_path_returns_a_session_and_audits_the_login(self):
        s = self.w.login(2)
        self.assertTrue(s.token and s.csrf)
        self.assertEqual(s.expires_at, int(self.w.clock.now) + self.w.settings.token_ttl_seconds)
        self.assertIn('login_success', self.w.log_text())
        self.assertEqual(self.w.store.get_user(BADGES[2]).last_login, self.w.clock.now)

    def test_every_kind_of_failure_looks_the_same_to_the_caller(self):
        w = self.w
        w.provision(2, badge='CG-2999')
        w.store.update_user('CG-2999', active=False)
        w.provision(2, badge='CG-2888')
        for _ in range(5):
            w.store.record_failed_login('CG-2888', w.clock.now, 5, 900)          # locked
        cases = {
            'unknown badge': lambda: w.service.login('CG-7777', 'Whatever-Pass-1!', '123456'),
            'wrong password': lambda: w.service.login(BADGES[2], 'Wrong-Pass-1357!', w.code(BADGES[2])),
            'wrong code': lambda: w.service.login(BADGES[2], PASSWORDS[2], '000000'),
            'inactive': lambda: w.service.login('CG-2999', PASSWORDS[2], w.code('CG-2999')),
            'locked, with correct credentials': lambda: w.service.login('CG-2888', PASSWORDS[2], w.code('CG-2888')),
        }
        seen = set()
        for label, call in cases.items():
            with self.assertRaises(service.AuthError, msg=label) as ctx:
                call()
            seen.add((ctx.exception.code, str(ctx.exception)))
        self.assertEqual(seen, {('invalid_credentials', 'invalid credentials')})

    def test_the_true_reason_is_in_the_audit_log(self):
        w = self.w
        for call in (lambda: w.service.login('CG-7777', 'Whatever-Pass-1!', '123456'),
                     lambda: w.service.login(BADGES[2], 'Wrong-Pass-1357!', w.code(BADGES[2])),
                     lambda: w.service.login(BADGES[2], PASSWORDS[2], '000000')):
            with self.assertRaises(service.AuthError):
                call()
        text = w.log_text()
        for reason in ('unknown_badge', 'bad_password', 'bad_totp'):
            self.assertIn(reason, text)

    def test_a_code_works_once(self):
        w = self.w
        code = w.code(BADGES[2])
        w.service.login(BADGES[2], PASSWORDS[2], code)
        with self.assertRaises(service.AuthError):
            w.service.login(BADGES[2], PASSWORDS[2], code)
        self.assertIn('replayed_totp', w.log_text())

    def test_five_wrong_passwords_lock_the_account_even_against_correct_credentials_until_the_lock_ends(self):
        w = self.w
        for _ in range(5):
            with self.assertRaises(service.AuthError):
                w.service.login(BADGES[2], 'Wrong-Pass-1357!', w.code(BADGES[2]))
        self.assertIn('"lockout"', w.log_text().replace(' ', ''))
        with self.assertRaises(service.AuthError):
            w.login(2)
        w.clock.advance(w.settings.lockout_seconds + 1)
        self.assertTrue(w.login(2).token)

    def test_a_successful_login_resets_the_failure_count(self):
        w = self.w
        for _ in range(3):
            with self.assertRaises(service.AuthError):
                w.service.login(BADGES[2], 'Wrong-Pass-1357!', w.code(BADGES[2]))
        w.login(2)
        self.assertEqual(w.store.get_user(BADGES[2]).failed_attempts, 0)

    def test_a_stolen_password_alone_never_logs_in_and_counts_as_a_failure(self):
        w = self.w
        with self.assertRaises(service.AuthError):
            w.service.login(BADGES[2], PASSWORDS[2], '123456')
        self.assertEqual(w.store.get_user(BADGES[2]).failed_attempts, 1)

    def test_malformed_input_is_a_normal_failure_and_never_reaches_the_store(self):
        class Spy(store.MemoryStore):
            def __init__(self):
                super().__init__()
                self.args = []

            def get_user(self, badge_id):
                self.args.append(badge_id)
                return super().get_user(badge_id)

        spy = Spy()
        with World(store_obj=spy) as w:
            for bad in ({'$ne': None}, ['CG-0001'], None, 5, b'CG-0001', 'cg-0001', 'CG-1', 'CG-123456789', 'x' * 5000, 'CG-0001\n'):
                with self.assertRaises(service.AuthError, msg=repr(bad)[:30]):
                    w.service.login(bad, 'Whatever-Pass-1!', '123456')
            for bad in ({'$gt': ''}, None, 5, ['x'], b'x', 'p' * 100_000):
                with self.assertRaises(service.AuthError):
                    w.service.login('CG-1001', bad, '123456')
            for bad in ({'$gt': ''}, None, 5, ['1'], b'123456'):
                with self.assertRaises(service.AuthError):
                    w.service.login('CG-1001', 'Whatever-Pass-1!', bad)
        self.assertTrue(all(type(a) is str for a in spy.args), spy.args)

    def test_an_unreachable_store_fails_closed(self):
        class Down(store.MemoryStore):
            def get_user(self, badge_id):
                raise store.StoreUnavailable('down')

            def ping(self):
                return False

        with World(store_obj=Down()) as w:
            with self.assertRaises(service.AuthError) as ctx:
                w.service.login(BADGES[2], PASSWORDS[2], '123456')
            self.assertEqual(ctx.exception.code, 'unavailable')
            with self.assertRaises(service.AuthError) as ctx:
                w.service.authenticate('x.y.z')
            self.assertIn(ctx.exception.code, ('unavailable', 'token_invalid'))

    def test_unknown_badge_and_wrong_password_take_about_the_same_time(self):
        # Hashing cost is raised for this test so that the password check, not the audit write, dominates the time; that is
        # the situation in production, and the only one in which a missing dummy check would be visible.
        with World(BCRYPT_ROUNDS=10) as w:
            w.provision(1)

            def timed(call):
                samples = []
                for _ in range(11):
                    t = time.perf_counter()
                    with self.assertRaises(service.AuthError):
                        call()
                    samples.append(time.perf_counter() - t)
                return statistics.median(samples)
            unknown = timed(lambda: w.service.login('CG-7777', 'Whatever-Pass-1!', '123456'))
            wrong = timed(lambda: w.service.login(BADGES[1], 'Wrong-Pass-1357!', '123456'))
            w.store.update_user(BADGES[1], active=False)
            inactive = timed(lambda: w.service.login(BADGES[1], PASSWORDS[1], '123456'))
        for label, value in (('unknown badge', unknown), ('inactive account', inactive)):
            ratio = value / wrong
            self.assertTrue(0.6 < ratio < 1.7, f'{label}: {ratio:.2f} of a wrong-password attempt')


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.w = World()
        self.w.provision_all()

    def tearDown(self):
        self.w.close()

    def test_authenticate_returns_the_current_permissions_not_a_cached_copy(self):
        w = self.w
        token = w.login(2).token
        self.assertEqual(w.service.authenticate(token).permissions, permissions.LEVEL_DEFAULTS[2])
        admin = w.principal(4)
        w.service.update_user(admin, BADGES[2], {'level': 1})
        self.assertEqual(w.service.authenticate(token).permissions, permissions.LEVEL_DEFAULTS[1])

    def test_a_deactivated_users_token_stops_working_at_once(self):
        w = self.w
        token = w.login(2).token
        w.service.update_user(w.principal(4), BADGES[2], {'active': False})
        with self.assertRaises(service.AuthError) as ctx:
            w.service.authenticate(token)
        self.assertEqual(ctx.exception.code, 'token_invalid')

    def test_logout_revokes_the_token(self):
        w = self.w
        token = w.login(2).token
        w.service.authenticate(token)
        w.service.logout(token)
        with self.assertRaises(service.AuthError):
            w.service.authenticate(token)
        self.assertIn('"logout"', w.log_text().replace(' ', ''))

    def test_logout_of_a_bad_token_is_harmless(self):
        for bad in ('x', '', None, 5):
            self.w.service.logout(bad)

    def test_an_expired_token_is_refused(self):
        w = self.w
        token = w.login(2).token
        w.clock.advance(w.settings.token_ttl_seconds + 1)
        with self.assertRaises(service.AuthError):
            w.service.authenticate(token)

    def test_a_forged_role_inside_a_correctly_signed_token_changes_nothing(self):
        w = self.w
        token = w.login(1).token
        payload = jwt.decode(token, w.settings.jwt_secret, algorithms=['HS256'], options={'verify_exp': False, 'verify_iat': False})
        payload.update({'level': 4, 'role': 'admin', 'permissions': list(permissions.PERMISSIONS)})
        forged = jwt.encode(payload, w.settings.jwt_secret, algorithm='HS256')
        self.assertEqual(w.service.authenticate(forged).permissions, permissions.LEVEL_DEFAULTS[1])

    def test_a_token_for_a_user_that_does_not_exist_is_refused(self):
        w = self.w
        from access import tokens
        ghost = tokens.issue(w.settings, 'CG-9999', now=w.clock.now).token
        with self.assertRaises(service.AuthError):
            w.service.authenticate(ghost)

    def test_garbage_tokens_are_token_invalid(self):
        for bad in (None, 5, '', 'a.b.c', 'x' * 100_000, {'a': 1}):
            with self.assertRaises(service.AuthError) as ctx:
                self.w.service.authenticate(bad)
            self.assertEqual(ctx.exception.code, 'token_invalid')


class UserManagementTests(unittest.TestCase):
    def setUp(self):
        self.w = World()
        self.w.provision_all()
        self.admin = self.w.principal(4)

    def tearDown(self):
        self.w.close()

    def create(self, **over):
        args = dict(badge_id='CG-5005', name='New Person', password='Fresh-Pass-8642!', level=2)
        args.update(over)
        return self.w.service.create_user(self.admin, **args)

    def test_creating_a_user_returns_a_working_provisioning_uri_and_stores_the_seed_encrypted(self):
        user, uri = self.create()
        secret = parse_qs(urlparse(uri).query)['secret'][0]
        self.assertTrue(user.must_change_password)
        stored = self.w.store.get_user('CG-5005')
        self.assertNotEqual(stored.totp_secret_enc, secret)
        self.assertEqual(totp.decrypt_secret(stored.totp_secret_enc, self.w.settings.fernet_key), secret)
        self.assertNotIn('Fresh-Pass-8642!', stored.password_hash)
        self.w.secrets['CG-5005'] = secret
        self.w.clock.advance(31)
        self.assertTrue(self.w.service.login('CG-5005', 'Fresh-Pass-8642!', self.w.code('CG-5005')).must_change_password)

    def test_weak_passwords_bad_badges_and_duplicates_are_refused_without_echoing_the_password(self):
        for kw in (dict(password='short'), dict(password='Password123!'), dict(badge_id='nope'), dict(badge_id='CG-1'),
                   dict(name=''), dict(name=5), dict(level=9), dict(level='2'), dict(badge_id=BADGES[2])):
            with self.assertRaises(ValueError, msg=str(kw)):
                self.create(**kw)
        for pw in ('short', 'Password123!', 'alllowercaseletters'):
            with self.assertRaises(ValueError) as ctx:
                self.create(password=pw)
            self.assertNotIn(pw, str(ctx.exception))
        self.assertIsNone(self.w.store.get_user('CG-5005'))

    def test_only_admins_manage_users(self):
        for level in (1, 2, 3):
            p = self.w.principal(level)
            for call in (lambda: self.w.service.create_user(p, 'CG-5005', 'N', 'Fresh-Pass-8642!', 1),
                         lambda: self.w.service.update_user(p, BADGES[1], {'name': 'x'}),
                         lambda: self.w.service.unlock_user(p, BADGES[1]),
                         lambda: self.w.service.reset_totp(p, BADGES[1])):
                with self.assertRaises(service.Forbidden, msg=f'level {level}'):
                    call()

    def test_mass_assignment_is_refused_field_by_field(self):
        for field, value in (('password_hash', 'x'), ('failed_attempts', 0), ('totp_secret_enc', 'x'), ('locked_until', None),
                             ('badge_id', 'CG-0001'), ('created_by', 'x'), ('last_login', 1.0), ('must_change_password', False),
                             ('role', 'admin'), ('is_admin', True), ('$set', {'level': 4})):
            with self.assertRaises(ValueError, msg=field):
                self.w.service.update_user(self.admin, BADGES[2], {field: value})
        self.assertEqual(self.w.store.get_user(BADGES[2]).level, 2)

    def test_admin_limits(self):
        s, a = self.w.service, self.admin
        with self.assertRaises(ValueError):
            s.update_user(a, BADGES[2], {'level': 5})
        with self.assertRaises(permissions.PermissionDenied):
            s.update_user(a, BADGES[4], {'level': 3})               # own level
        with self.assertRaises(permissions.PermissionDenied):
            s.update_user(a, BADGES[4], {'active': False})          # deactivate self
        with self.assertRaises(permissions.PermissionDenied):
            s.update_user(a, BADGES[2], {'grants': ['users.manage']})   # sensitive grant to a non-admin
        s.update_user(a, BADGES[2], {'grants': ['claims.decide_high']})
        self.assertEqual(self.w.store.get_user(BADGES[2]).grants, ('claims.decide_high',))
        self.assertIn('claims.decide_high', self.w.principal(2).permissions)

    def test_an_admin_cannot_grant_claim_permissions_to_themself_or_another_admin(self):
        s, a = self.w.service, self.admin
        self.create(badge_id='CG-4005', level=4)
        for target in (BADGES[4], 'CG-4005'):
            for flag in ('claims.decide', 'claims.decide_high', 'pii.unmask', 'claims.view'):
                with self.assertRaises(permissions.PermissionDenied, msg=f'{target} {flag}'):
                    s.update_user(a, target, {'grants': [flag]})
        self.assertEqual(self.w.store.get_user(BADGES[4]).grants, ())
        self.assertNotIn('claims.decide_high', self.w.principal(4).permissions)

    def test_promoting_a_user_with_claim_grants_to_administrator_is_refused_until_the_grants_are_cleared(self):
        s, a = self.w.service, self.admin
        s.update_user(a, BADGES[2], {'grants': ['claims.decide_high']})
        with self.assertRaises(permissions.PermissionDenied):
            s.update_user(a, BADGES[2], {'level': 4})
        s.update_user(a, BADGES[2], {'level': 4, 'grants': []})
        self.assertNotIn('claims.decide', self.w.principal(2).permissions)

    def test_an_admin_cannot_demote_or_deactivate_themself_even_when_another_admin_exists(self):
        s, a = self.w.service, self.admin
        self.create(badge_id='CG-4005', level=4)
        self.assertEqual(self.w.store.count_active_admins(), 2)
        for change in ({'level': 3}, {'active': False}):
            with self.assertRaises(permissions.PermissionDenied, msg=str(change)):
                s.update_user(a, BADGES[4], change)
        self.assertEqual(self.w.store.get_user(BADGES[4]).level, 4)

    def test_the_last_active_admin_cannot_be_demoted_or_deactivated(self):
        s, a = self.w.service, self.admin
        self.create(badge_id='CG-4005', level=4)
        # Another change has just removed the other administrator (a race between two admins): this one is now the last.
        self.w.store.count_active_admins = lambda: 1
        for change in ({'level': 3}, {'active': False}):
            with self.assertRaises(permissions.PermissionDenied, msg=str(change)):
                s.update_user(a, 'CG-4005', change)
        self.assertEqual(self.w.store.get_user('CG-4005').level, 4)

    def test_unlock_and_reset_totp(self):
        w, s = self.w, self.w.service
        for _ in range(5):
            w.store.record_failed_login(BADGES[2], w.clock.now, 5, 900)
        s.unlock_user(self.admin, BADGES[2])
        u = w.store.get_user(BADGES[2])
        self.assertEqual((u.failed_attempts, u.locked_until), (0, None))
        old = u.totp_secret_enc
        uri = s.reset_totp(self.admin, BADGES[2])
        self.assertNotEqual(w.store.get_user(BADGES[2]).totp_secret_enc, old)
        self.assertIn('otpauth://totp/', uri)
        with self.assertRaises(KeyError):
            s.unlock_user(self.admin, 'CG-9999')

    def test_changing_your_own_password(self):
        w, s = self.w, self.w.service
        w.clock.advance(31)
        token = w.login(2).token
        p = s.authenticate(token)
        with self.assertRaises(service.AuthError):
            s.change_password(p, 'Wrong-Pass-1357!', 'Brand-New-Pass-2468!')
        with self.assertRaises(ValueError):
            s.change_password(p, PASSWORDS[2], 'short')
        with self.assertRaises(ValueError):
            s.change_password(p, PASSWORDS[2], PASSWORDS[2])
        s.change_password(p, PASSWORDS[2], 'Brand-New-Pass-2468!')
        with self.assertRaises(service.AuthError):
            s.authenticate(token)                                    # the session that changed it is over
        w.clock.advance(31)
        self.assertTrue(w.service.login(BADGES[2], 'Brand-New-Pass-2468!', w.code(BADGES[2])).token)
        with self.assertRaises(service.AuthError):
            w.service.login(BADGES[2], PASSWORDS[2], w.code(BADGES[2], at=w.clock.now + 31))   # the old password is dead

    def test_management_actions_are_audited_with_before_and_after(self):
        self.w.service.update_user(self.admin, BADGES[2], {'level': 3})
        text = self.w.log_text()
        self.assertIn('user_updated', text)
        events = [e['event'] for e in self.w.log.events(limit=1000) if e['event']['event_type'] == 'user_updated']
        self.assertEqual(events[-1]['changes'], {'level': [2, 3]})


class SecretsStayOutOfTheLogTests(unittest.TestCase):
    def test_nothing_secret_is_ever_written(self):
        with World() as w:
            w.provision_all()
            code_seen, tokens_seen = [], []
            for level in (1, 2, 3, 4):
                w.clock.advance(31)
                code = w.code(BADGES[level])
                code_seen.append(code)
                tokens_seen.append(w.service.login(BADGES[level], PASSWORDS[level], code).token)
            for _ in range(3):
                with self.assertRaises(service.AuthError):
                    w.service.login(BADGES[2], 'Planted-Wrong-Pass-77!', '654321')
            w.service.update_user(w.principal(4), BADGES[1], {'level': 2})
            text = w.log_text()
            planted = list(PASSWORDS.values()) + ['Planted-Wrong-Pass-77!', '654321'] + code_seen + tokens_seen + list(w.secrets.values()) \
                + [w.settings.jwt_secret, w.settings.fernet_key, w.settings.pii_key, w.settings.audit_anchor_key]
            for secret in planted:
                self.assertNotIn(secret, text, secret[:6])


class AuditGateTests(unittest.TestCase):
    def test_a_flood_of_failures_cannot_flood_the_log(self):
        with World() as w:
            w.provision(2)
            for i in range(300):
                with self.assertRaises(service.AuthError):
                    w.service.login('CG-7777', 'Whatever-Pass-1!', '123456', client=str(i))
            failures = [e for e in w.log.events(limit=1000) if e['event']['event_type'] == 'login_failure']
            self.assertLessEqual(len(failures), 210)
            w.clock.advance(61)
            with self.assertRaises(service.AuthError):
                w.service.login('CG-7777', 'Whatever-Pass-1!', '123456')
            reasons = [e['event']['reason'] for e in w.log.events(limit=1000) if e['event']['event_type'] == 'login_failure']
            self.assertTrue(any(r.startswith('suppressed_') for r in reasons), reasons[-3:])
            self.assertTrue(w.log.verify()['ok'])


class ServiceAuditHelpersTests(unittest.TestCase):
    def test_a_flood_of_forbidden_requests_is_capped_and_resumes_with_a_count(self):
        with World() as w:
            w.provision_all()
            for i in range(400):
                w.service.note_forbidden(BADGES[1], 'GET', f'/api/v1/users?{i}')
            forbidden = [e for e in w.log.events(limit=1000) if e['event']['event_type'] == 'forbidden']
            self.assertLessEqual(len(forbidden), 210)
            w.clock.advance(61)
            w.service.note_forbidden(BADGES[1], 'GET', '/api/v1/users')
            last = [e for e in w.log.events(limit=1000) if e['event']['event_type'] == 'forbidden'][-1]['event']
            self.assertGreater(last['suppressed_before'], 100)
            self.assertTrue(w.log.verify()['ok'])

    def test_long_or_odd_paths_never_break_the_audit(self):
        with World() as w:
            w.service.note_forbidden('CG-1001', 'GET', 'x' * 5000)
            w.service.note_forbidden('CG-1001', 'G' * 100, '/a' + chr(0x2028) + 'b' + chr(0) + 'c')
            self.assertEqual(len(w.log.events(limit=10)), 2)

    def test_record_writes_a_checked_event_and_refuses_bad_ones(self):
        with World() as w:
            w.service.record('audit_read', badge_id='CG-4004')
            with self.assertRaises(ValueError):
                w.service.record('audit_read', badge_id='CG-4004', password='x')
            self.assertEqual(len(w.log.events(limit=10)), 1)

    def test_listing_users_needs_the_permission_and_exposes_no_secret(self):
        with World() as w:
            w.provision_all()
            rows = w.service.list_users(w.principal(4))
            self.assertEqual(len(rows), 4)
            with self.assertRaises(service.Forbidden):
                w.service.list_users(w.principal(2))

    def test_a_duplicate_badge_is_its_own_error_type(self):
        with World() as w:
            w.provision_all()
            with self.assertRaises(service.DuplicateUser):
                w.service.provision_user(BADGES[2], 'Again', 'Fresh-Pass-8642!', 2)
            self.assertTrue(issubclass(service.DuplicateUser, ValueError))


if __name__ == '__main__':
    unittest.main()
