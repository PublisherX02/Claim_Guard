"""The behaviour every UserStore must have. The in-memory store and the MongoDB store both run this exact suite, so the fast
local runs are checked against the real database's behaviour instead of trusting a hand-written fake."""
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import store as s

NOW = 1_700_000_000.0


def make_user(badge='CG-0001', level=2, **over):
    base = dict(badge_id=badge, name='Test User', password_hash='$2b$04$hash', totp_secret_enc='enc', level=level,
                grants=(), revokes=(), active=True, failed_attempts=0, locked_until=None, must_change_password=False,
                created_by='CG-9000', created_at=NOW, last_login=None)
    base.update(over)
    return s.User(**base)


def run_parallel(n, fn):
    """Run fn(i) on n threads released together; return the list of results in thread order."""
    barrier, results = threading.Barrier(n), [None] * n

    def work(i):
        barrier.wait()
        results[i] = fn(i)
    threads = [threading.Thread(target=work, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return results


class StoreContract:
    """Mixin: a TestCase supplies make_store(). Every test builds its own store so tests cannot influence one another."""

    def make_store(self):
        raise NotImplementedError

    def test_ping(self):
        self.assertTrue(self.make_store().ping())

    def test_create_and_get_round_trip(self):
        st = self.make_store()
        u = make_user('CG-0001', level=3, grants=('pii.unmask',), revokes=('claims.recheck',))
        st.create_user(u)
        got = st.get_user('CG-0001')
        self.assertEqual(got, u)
        self.assertIsInstance(got.grants, tuple)

    def test_duplicate_badge_raises(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        with self.assertRaises(s.DuplicateBadge):
            st.create_user(make_user('CG-0001', name='Other'))
        self.assertEqual(st.get_user('CG-0001').name, 'Test User')

    def test_unknown_badge_is_none(self):
        self.assertIsNone(self.make_store().get_user('CG-9999'))

    def test_returned_users_are_copies(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        st.get_user('CG-0001').name = 'Changed'
        st.list_users()[0].name = 'Changed'
        self.assertEqual(st.get_user('CG-0001').name, 'Test User')

    def test_update_changes_only_allowed_fields(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        u = st.update_user('CG-0001', name='New Name', level=3, active=False, grants=('pii.unmask',), must_change_password=True)
        self.assertEqual((u.name, u.level, u.active, u.grants, u.must_change_password), ('New Name', 3, False, ('pii.unmask',), True))
        self.assertEqual(st.get_user('CG-0001'), u)
        for bad in ('badge_id', 'created_by', 'created_at', 'last_login', 'role', 'is_admin', '$set', 'password'):
            with self.assertRaises(ValueError, msg=bad):
                st.update_user('CG-0001', **{bad: 'x'})
        with self.assertRaises(KeyError):
            st.update_user('CG-9999', name='x')

    def test_failed_logins_count_and_lock_at_the_limit(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        for i in range(1, 5):
            u = st.record_failed_login('CG-0001', NOW, 5, 900)
            self.assertEqual((u.failed_attempts, u.locked_until), (i, None))
        u = st.record_failed_login('CG-0001', NOW, 5, 900)
        self.assertEqual((u.failed_attempts, u.locked_until), (5, NOW + 900))

    def test_more_failures_do_not_extend_a_lock(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        for _ in range(5):
            st.record_failed_login('CG-0001', NOW, 5, 900)
        u = st.record_failed_login('CG-0001', NOW + 100, 5, 900)
        self.assertEqual(u.locked_until, NOW + 900)
        self.assertEqual(u.failed_attempts, 6)

    def test_a_failure_after_the_lock_expired_starts_a_new_count(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        for _ in range(5):
            st.record_failed_login('CG-0001', NOW, 5, 900)
        u = st.record_failed_login('CG-0001', NOW + 901, 5, 900)
        self.assertEqual((u.failed_attempts, u.locked_until), (1, None))

    def test_success_resets_the_counter_and_records_the_time(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        for _ in range(3):
            st.record_failed_login('CG-0001', NOW, 5, 900)
        st.record_successful_login('CG-0001', NOW + 5)
        u = st.get_user('CG-0001')
        self.assertEqual((u.failed_attempts, u.locked_until, u.last_login), (0, None, NOW + 5))

    def test_a_totp_step_is_accepted_once_per_badge(self):
        st = self.make_store()
        self.assertTrue(st.mark_totp_used('CG-0001', 100, NOW + 90))
        self.assertFalse(st.mark_totp_used('CG-0001', 100, NOW + 90))
        self.assertTrue(st.mark_totp_used('CG-0001', 101, NOW + 90))
        self.assertTrue(st.mark_totp_used('CG-0002', 100, NOW + 90))

    def test_fifty_parallel_submissions_of_one_step_yield_exactly_one_success(self):
        st = self.make_store()
        results = run_parallel(50, lambda i: st.mark_totp_used('CG-0001', 777, NOW + 90))
        self.assertEqual(results.count(True), 1)
        self.assertEqual(results.count(False), 49)

    def test_parallel_failed_logins_lose_no_updates(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))

        def hammer(i):
            for _ in range(10):
                st.record_failed_login('CG-0001', NOW, 5, 900)
        run_parallel(50, hammer)
        u = st.get_user('CG-0001')
        self.assertEqual(u.failed_attempts, 500)
        self.assertEqual(u.locked_until, NOW + 900)

    def test_revocation_lasts_until_the_token_would_have_expired(self):
        st = self.make_store()
        self.assertFalse(st.is_revoked('jti-1', NOW))
        st.revoke_token('jti-1', NOW + 100)
        self.assertTrue(st.is_revoked('jti-1', NOW + 50))
        self.assertTrue(st.is_revoked('jti-1', NOW + 99))
        self.assertFalse(st.is_revoked('jti-1', NOW + 101))
        self.assertFalse(st.is_revoked('jti-2', NOW + 50))
        st.revoke_token('jti-1', NOW + 100)          # revoking twice is harmless

    def test_count_active_admins_counts_only_active_level_four(self):
        st = self.make_store()
        self.assertEqual(st.count_active_admins(), 0)
        st.create_user(make_user('CG-0001', level=4))
        st.create_user(make_user('CG-0002', level=4, active=False))
        st.create_user(make_user('CG-0003', level=3))
        self.assertEqual(st.count_active_admins(), 1)
        st.update_user('CG-0002', active=True)
        self.assertEqual(st.count_active_admins(), 2)

    def test_list_users_returns_everyone(self):
        st = self.make_store()
        for i in range(1, 4):
            st.create_user(make_user(f'CG-000{i}'))
        self.assertEqual(sorted(u.badge_id for u in st.list_users()), ['CG-0001', 'CG-0002', 'CG-0003'])

    def test_non_string_identifiers_are_rejected_before_they_reach_the_database(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        evil = [{'$ne': None}, {'$gt': ''}, ['CG-0001'], 5, None, b'CG-0001', {'$regex': '.*'}]
        calls = [lambda b: st.get_user(b), lambda b: st.update_user(b, name='x'),
                 lambda b: st.record_failed_login(b, NOW, 5, 900), lambda b: st.record_successful_login(b, NOW),
                 lambda b: st.mark_totp_used(b, 1, NOW + 90), lambda b: st.revoke_token(b, NOW + 90),
                 lambda b: st.is_revoked(b, NOW)]
        for call in calls:
            for value in evil:
                with self.assertRaises(TypeError, msg=repr(value)):
                    call(value)
        self.assertEqual(st.get_user('CG-0001').failed_attempts, 0)

    def test_wrongly_typed_fields_are_rejected_on_create(self):
        st = self.make_store()
        bad_fields = [dict(name=5), dict(name={'$set': 'x'}), dict(password_hash=None), dict(totp_secret_enc=b'x'),
                      dict(level='2'), dict(level=True), dict(level=0), dict(level=5), dict(grants='abc'), dict(grants=(1,)),
                      dict(revokes=({'a': 1},)), dict(active='yes'), dict(failed_attempts=-1), dict(failed_attempts=1.5),
                      dict(locked_until='soon'), dict(must_change_password=1), dict(created_by=None), dict(created_at='now')]
        for i, over in enumerate(bad_fields):
            with self.assertRaises(TypeError, msg=repr(over)):
                st.create_user(make_user(f'CG-{1000 + i}', **over))
        self.assertEqual(st.list_users(), [])

    def test_wrongly_typed_values_are_rejected_on_update(self):
        st = self.make_store()
        st.create_user(make_user('CG-0001'))
        for field, value in (('name', 5), ('level', '3'), ('level', True), ('level', 9), ('active', 'no'), ('grants', 'x'),
                             ('grants', [{'$ne': 1}]), ('failed_attempts', -3), ('locked_until', 'x'),
                             ('password_hash', {'$set': 1}), ('must_change_password', None)):
            with self.assertRaises(TypeError, msg=f'{field}={value!r}'):
                st.update_user('CG-0001', **{field: value})
        self.assertEqual(st.get_user('CG-0001'), make_user('CG-0001'))

    def test_create_user_requires_a_user_object(self):
        st = self.make_store()
        for bad in ({'badge_id': 'CG-0001'}, None, 'CG-0001'):
            with self.assertRaises(TypeError):
                st.create_user(bad)
