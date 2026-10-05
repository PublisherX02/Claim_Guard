"""The MongoDB queue store must pass the same contract suite as the in-memory twin, against a real server.

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
from workqueue import store as s
from queue_store_contract import StoreContract, make_doc, NOW

MONGO_URI = os.environ.get('MONGO_URI')
SKIP = 'MONGO_URI not set: the MongoDB queue contract tests were NOT run'


class MongoIsConfigured(unittest.TestCase):
    def test_the_database_is_configured_when_it_is_required(self):
        if os.environ.get('REQUIRE_MONGO') == '1':
            self.assertTrue(MONGO_URI, 'REQUIRE_MONGO=1 but MONGO_URI is not set')
        elif not MONGO_URI:
            self.skipTest(SKIP)


def fresh_store(made):
    from workqueue.store_mongo import MongoQueueStore
    st = MongoQueueStore(MONGO_URI, 'claimguard_test_' + uuid.uuid4().hex[:12])
    st.ensure_indexes()
    made.append(st)
    return st


@unittest.skipUnless(MONGO_URI, SKIP)
class MongoQueueStoreContract(StoreContract, unittest.TestCase):
    def setUp(self):
        self._made = []
        super().setUp()

    def make_store(self):
        return fresh_store(self._made)

    def tearDown(self):
        for st in self._made:
            st.drop_database()
            st.close()


@unittest.skipUnless(MONGO_URI, SKIP)
class MongoSpecifics(unittest.TestCase):
    def setUp(self):
        self._made = []
        self.st = fresh_store(self._made)

    def tearDown(self):
        for st in self._made:
            st.drop_database()
            st.close()

    def indexes(self, collection):
        return dict(self.st.raw_db[collection].index_information())

    def test_the_indexes_that_make_the_guarantees_exist(self):
        claims = self.indexes('claims')
        self.assertTrue(any(i.get('unique') and i['key'] == [('claim_id', 1), ('version', 1)] for i in claims.values()))
        self.assertTrue(any(i.get('unique') and i['key'] == [('claim_id', 1), ('input_hash', 1), ('receipt.rule_pack_hash', 1)] for i in claims.values()))
        configs = self.indexes('configs')
        self.assertTrue(any(i.get('unique') and i['key'] == [('version', 1)] for i in configs.values()))
        counters = self.indexes('counters')
        self.assertTrue(any(i.get('unique') and i['key'] == [('counter', 1), ('window', 1)] for i in counters.values()))
        self.assertTrue(any(i['key'] == [('expires_at', 1)] and i.get('expireAfterSeconds') == 0 for i in counters.values()))
        cache = self.indexes('cache')
        self.assertTrue(any(i['key'] == [('expires_at', 1)] and i.get('expireAfterSeconds') == 0 for i in cache.values()))

    def test_ensure_indexes_can_run_twice(self):
        self.st.ensure_indexes()
        self.st.ensure_indexes()

    def test_a_stored_claim_has_no_operator_looking_keys_at_the_top_and_keeps_its_shape(self):
        self.st.put_triaged(make_doc())
        doc = self.st.raw_db['claims'].find_one({'claim_id': 'C1'})
        self.assertTrue(all(not k.startswith('$') for k in doc))
        self.assertEqual((doc['state'], doc['version'], doc['enqueue_pending']), ('triaged', 1, True))

    def test_the_lease_filter_is_in_the_update_not_a_read_then_write(self):
        # a hand-made second writer changes the state between the twin's read and its write; the database refuses the stale move
        self.st.put_triaged(make_doc())
        self.st.transition('C1', 1, 'triaged', 'ready', 'system:t', NOW + 1)
        self.st.raw_db['claims'].update_one({'claim_id': 'C1'}, {'$set': {'state': 'dead_lettered'}})
        self.assertIsNone(self.st.lease('C1', 1, 'B1', NOW + 2, NOW + 100))
        self.assertEqual(self.st.get('C1')['state'], 'dead_lettered')


class MongoUnreachable(unittest.TestCase):
    """No server needed: these prove that a dead database means refusal, not silence."""

    def setUp(self):
        from workqueue.store_mongo import MongoQueueStore
        self.st = MongoQueueStore('mongodb://127.0.0.1:1', 'nothing', timeout_ms=300)

    def tearDown(self):
        self.st.close()

    def test_every_call_is_store_unavailable(self):
        st = self.st
        calls = [lambda: st.put_triaged(make_doc()), lambda: st.get('C1'),
                 lambda: st.transition('C1', 1, 'triaged', 'ready', 'a', NOW), lambda: st.pending_outbox(5),
                 lambda: st.clear_outbox('C1', 1), lambda: st.by_state('ready'), lambda: st.counts(),
                 lambda: st.lease('C1', 1, 'B1', NOW, NOW + 5), lambda: st.inbox('B1'), lambda: st.heartbeat('B1', NOW, NOW + 5),
                 lambda: st.expired(NOW), lambda: st.claims_for_patient('P1'), lambda: st.put_config({'version': 1}, 0),
                 lambda: st.latest_config(), lambda: st.config_history(), lambda: st.append_deal({'deal_id': 'D'}),
                 lambda: st.get_deal('D'), lambda: st.deals(), lambda: st.add_dead_letter({'dead_id': 'X'}),
                 lambda: st.dead_letters(), lambda: st.pop_dead_letter('X'), lambda: st.cache_get('k'),
                 lambda: st.cache_put('k', 'v'), lambda: st.bump('c', 'w', 3)]
        self.assertFalse(st.ping())
        for i, call in enumerate(calls):
            with self.assertRaises(s.StoreUnavailable, msg=f'call {i}'):
                call()


@unittest.skipUnless(MONGO_URI, SKIP)
class MongoHistoryTests(unittest.TestCase):
    """The database-backed history must return exactly what InMemoryHistory returns, for every one of the public claims."""

    def test_earlier_claims_are_identical_to_the_in_memory_history_for_all_public_claims(self):
        from claim_history import InMemoryHistory
        from engine_core import load_jsonl
        from workqueue.store_mongo import MongoHistory
        made = []
        try:
            st = fresh_store(made)
            claims = []
            for split in ('development', 'validation', 'stress'):
                claims += [dict(c, claim_id=f'{split[:3]}-{c["claim_id"]}') for c in load_jsonl(str(ROOT / 'data' / split / 'claims.jsonl'))]
            self.assertGreaterEqual(len(claims), 600)
            # the public claims share no patients, so also compare on a copy where 40 patients own all the claims
            claims += [dict(c, claim_id='shr-' + c['claim_id'], patient_id=f'SHARED-{i % 40}') for i, c in enumerate(claims)]
            for i, c in enumerate(claims):
                self.assertTrue(st.put_triaged(make_doc(c['claim_id'], input_hash=f'h{i}') | {'claim': c}))
            mem, mongo = InMemoryHistory(claims), MongoHistory(st)
            nonempty = 0
            for c in claims:
                want = mem.earlier_claims(c)
                got = mongo.earlier_claims(c)
                self.assertEqual(sorted(x['claim_id'] for x in got), sorted(x['claim_id'] for x in want), c['claim_id'])
                nonempty += bool(want)
            self.assertGreater(nonempty, 0, 'the fixture must contain claims that have earlier claims')
        finally:
            for st in made:
                st.drop_database()
                st.close()

    def test_a_claim_without_a_patient_or_date_has_no_history(self):
        from workqueue.store_mongo import MongoHistory
        made = []
        try:
            st = fresh_store(made)
            h = MongoHistory(st)
            self.assertEqual(h.earlier_claims({'claim_id': 'X'}), [])
            self.assertEqual(h.earlier_claims({'claim_id': 'X', 'patient_id': 'P1', 'submission_date': 'garbage'}), [])
            self.assertEqual(h.earlier_claims('nope'), [])
        finally:
            for st in made:
                st.drop_database()
                st.close()


if __name__ == '__main__':
    unittest.main()
