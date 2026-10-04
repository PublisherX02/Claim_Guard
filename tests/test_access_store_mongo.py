"""The MongoDB user store must pass the same contract suite as the in-memory twin, against a real server.

MONGO_URI points at a throwaway server (docker compose up -d mongo, or a CI service container). With REQUIRE_MONGO=1 a missing
URI is a failure, never a silent skip: that is how CI proves these tests ran.
"""
import os
import sys
import unittest
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import store as s
from access_store_contract import StoreContract, make_user

MONGO_URI = os.environ.get('MONGO_URI')
SKIP = 'MONGO_URI not set: the MongoDB contract tests were NOT run'


class MongoIsConfigured(unittest.TestCase):
    def test_the_database_is_configured_when_it_is_required(self):
        if os.environ.get('REQUIRE_MONGO') == '1':
            self.assertTrue(MONGO_URI, 'REQUIRE_MONGO=1 but MONGO_URI is not set')
        elif not MONGO_URI:
            self.skipTest(SKIP)


@unittest.skipUnless(MONGO_URI, SKIP)
class MongoStoreContract(StoreContract, unittest.TestCase):
    def setUp(self):
        self._made = []

    def make_store(self):
        from access.store_mongo import MongoStore
        st = MongoStore(MONGO_URI, 'claimguard_test_' + uuid.uuid4().hex[:12])
        st.ensure_indexes()
        self._made.append(st)
        return st

    def tearDown(self):
        for st in self._made:
            st.drop_database()
            st.close()


@unittest.skipUnless(MONGO_URI, SKIP)
class MongoSpecifics(unittest.TestCase):
    def setUp(self):
        from access.store_mongo import MongoStore
        self.st = MongoStore(MONGO_URI, 'claimguard_test_' + uuid.uuid4().hex[:12])
        self.st.ensure_indexes()

    def tearDown(self):
        self.st.drop_database()
        self.st.close()

    def indexes(self, collection):
        return {name: info for name, info in self.st.raw_db[collection].index_information().items()}

    def test_the_indexes_that_make_the_guarantees_exist(self):
        users = self.indexes('users')
        self.assertTrue(any(i.get('unique') and i['key'] == [('badge_id', 1)] for i in users.values()))
        totp = self.indexes('used_totp')
        self.assertTrue(any(i.get('unique') and i['key'] == [('badge_id', 1), ('step', 1)] for i in totp.values()))
        self.assertTrue(any(i['key'] == [('expires_at', 1)] and i.get('expireAfterSeconds') == 0 for i in totp.values()))
        revoked = self.indexes('revoked_tokens')
        self.assertTrue(any(i.get('unique') and i['key'] == [('jti', 1)] for i in revoked.values()))
        self.assertTrue(any(i['key'] == [('expires_at', 1)] and i.get('expireAfterSeconds') == 0 for i in revoked.values()))

    def test_ensure_indexes_can_run_twice(self):
        self.st.ensure_indexes()
        self.st.ensure_indexes()

    def test_a_stored_user_has_no_operator_looking_keys_and_the_expected_shape(self):
        self.st.create_user(make_user('CG-0001', grants=('pii.unmask',)))
        doc = self.st.raw_db['users'].find_one({'badge_id': 'CG-0001'})
        self.assertTrue(all(not k.startswith('$') for k in doc))
        self.assertEqual(doc['grants'], ['pii.unmask'])
        self.assertEqual(doc['level'], 2)

    def test_the_password_hash_and_seed_are_stored_exactly_as_given(self):
        self.st.create_user(make_user('CG-0001', password_hash='$2b$04$abc', totp_secret_enc='gAAAA-enc'))
        u = self.st.get_user('CG-0001')
        self.assertEqual((u.password_hash, u.totp_secret_enc), ('$2b$04$abc', 'gAAAA-enc'))


class MongoUnreachable(unittest.TestCase):
    """No server needed: these prove that a dead database means refusal, not silence."""

    def setUp(self):
        from access.store_mongo import MongoStore
        self.st = MongoStore('mongodb://127.0.0.1:1', 'nothing', timeout_ms=300)

    def tearDown(self):
        self.st.close()

    def test_ping_is_false_and_every_other_call_is_store_unavailable(self):
        self.assertFalse(self.st.ping())
        calls = [lambda: self.st.get_user('CG-0001'), lambda: self.st.list_users(), lambda: self.st.create_user(make_user()),
                 lambda: self.st.update_user('CG-0001', name='x'), lambda: self.st.record_failed_login('CG-0001', 1.0, 5, 900),
                 lambda: self.st.record_successful_login('CG-0001', 1.0), lambda: self.st.mark_totp_used('CG-0001', 1, 99.0),
                 lambda: self.st.revoke_token('j', 99.0), lambda: self.st.is_revoked('j', 1.0), lambda: self.st.count_active_admins()]
        for call in calls:
            with self.assertRaises(s.StoreUnavailable):
                call()


if __name__ == '__main__':
    unittest.main()
