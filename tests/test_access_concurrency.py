"""Races. Each test starts real threads together and checks what must hold however they interleave.

They run on the in-memory store and, when MONGO_URI is set, on MongoDB: a race is decided by the database's atomic operations,
so only the real database can prove the guarantees.
"""
import json
import os
import sys
import threading
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import service
from access_api_world import ApiWorld
from access_store_contract import run_parallel
from access_world import BADGES, PASSWORDS
from test_access_attacks import MONGO_URI, mongo_world

API = '/api/v1'


class RaceBase:
    def new_world(self):
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

    def chain_ok(self):
        v = self.w.log.verify()
        self.assertTrue(v['ok'], v)
        return v

    def test_fifty_parallel_logins_with_one_valid_code_produce_exactly_one_session(self):
        w = self.w
        code = w.code(BADGES[2])

        def attempt(i):
            try:
                return w.service.login(BADGES[2], PASSWORDS[2], code, client=str(i))
            except service.AuthError:
                return None
        results = run_parallel(50, attempt)
        self.assertEqual(sum(r is not None for r in results), 1)
        self.chain_ok()

    def test_a_hundred_parallel_wrong_passwords_lock_the_account_and_the_log_stays_valid(self):
        w = self.w

        def attempt(i):
            try:
                w.service.login(BADGES[2], 'Wrong-Pass-1357!', '123456', client=str(i))
            except service.AuthError as e:
                return e.code
        codes = run_parallel(100, attempt)
        self.assertEqual(set(codes), {'invalid_credentials'})
        user = w.store.get_user(BADGES[2])
        self.assertIsNotNone(user.locked_until)
        self.assertGreaterEqual(user.failed_attempts, 5)
        events = w.log.events(limit=1000)
        self.assertTrue(any(e['event']['event_type'] == 'lockout' for e in events))
        self.assertLessEqual(len(events), 400)
        self.chain_ok()
        w.clock.advance(w.settings.lockout_seconds + 1)
        self.assertTrue(w.login(2).token)                               # and it unlocks, as it should

    def test_two_administrators_editing_one_account_at_once_leave_a_valid_record_and_a_complete_audit(self):
        w = self.w
        w.provision(4, badge='CG-4005')
        a, b = w.principal(4), w.service.authenticate(w.login(4, badge='CG-4005').token)
        target = BADGES[1]
        outcomes = []

        def edit(i):
            actor, levels = (a, (2, 3)) if i == 0 else (b, (3, 2))
            done = 0
            for n in range(15):
                try:
                    w.service.update_user(actor, target, {'level': levels[n % 2]})
                    done += 1
                except Exception as e:                                   # noqa: BLE001 -- count anything that is not a clean success
                    outcomes.append(type(e).__name__)
            return done
        done = run_parallel(2, edit)
        self.assertEqual(outcomes, [])
        self.assertIn(w.store.get_user(target).level, (2, 3))
        updates = [e for e in w.log.events(limit=1000) if e['event']['event_type'] == 'user_updated']
        self.assertEqual(len(updates), sum(done))
        self.chain_ok()

    def test_after_a_demotion_completes_no_later_request_gets_the_old_access(self):
        w = self.w
        reviewer = w.login_client(2)
        token = reviewer.cookies.get('cg_session')
        stop, log, lock = threading.Event(), [], threading.Lock()

        def hammer(_):
            c = w.client()
            c.cookies.set('cg_session', token)
            while not stop.is_set():
                started = time.monotonic()
                status = c.get(f'{API}/claims', params={'limit': 1}).status_code
                with lock:
                    log.append((started, status))
        threads = [threading.Thread(target=hammer, args=(i,)) for i in range(6)]
        for t in threads:
            t.start()
        time.sleep(0.6)
        w.store.update_user(BADGES[2], level=4)                          # level 4 has no claims.view
        demoted_at = time.monotonic()
        time.sleep(0.8)
        stop.set()
        for t in threads:
            t.join()
        before = [s for t, s in log if t < demoted_at - 0.05]
        after = [s for t, s in log if t > demoted_at]
        self.assertIn(200, before, 'the test never saw the old access, so it proved nothing')
        self.assertTrue(after)
        self.assertEqual(set(after), {403}, 'a request that started after the demotion still got the old access')

    def test_two_senior_reviewers_deciding_one_finding_at_once_are_both_recorded_in_order(self):
        w = self.w
        w.provision(3, badge='CG-3999')
        one, two = w.login_client(3), w.login_client(3, badge='CG-3999')
        claim, rule = w.high()
        body = {'action': 'confirm_issue', 'reason': 'Checked at the same moment'}

        def decide(i):
            return (one, two)[i].post(f'{API}/claims/{claim}/findings/{rule}/decision', json=body).status_code
        statuses = run_parallel(2, decide)
        self.assertEqual(statuses, [200, 200])
        rows = [json.loads(l)['event'] for l in w.review_log.path.read_text(encoding='utf-8').split('\n') if l.strip()]
        decisions = [e for e in rows if e.get('claim_id') == claim and e.get('rule_id') == rule]
        self.assertEqual({d['actor'] for d in decisions}, {BADGES[3], 'CG-3999'})
        self.assertEqual(len(decisions), 2)
        self.assertTrue(w.client() is not None)
        admin = w.login_client(4)
        self.assertTrue(admin.get(f'{API}/audit/verify').json()['review']['ok'])

    def test_parallel_reads_while_users_log_in_and_out_never_error(self):
        w = self.w
        clients = [w.login_client(level) for level in (1, 2, 3)]
        stop, errors = threading.Event(), []

        def read(i):
            c = clients[i % 3]
            while not stop.is_set():
                s = c.get(f'{API}/claims', params={'limit': 5}).status_code
                if s >= 500:
                    errors.append(s)

        def churn():
            for n in range(6):
                w.clock.advance(31)
                try:
                    s = w.login(2)
                    w.service.logout(s.token)
                except service.AuthError:
                    pass
        readers = [threading.Thread(target=read, args=(i,)) for i in range(6)]
        for t in readers:
            t.start()
        churn()
        stop.set()
        for t in readers:
            t.join()
        self.assertEqual(errors, [])
        self.chain_ok()


class MemoryRaces(RaceBase, unittest.TestCase):
    def new_world(self):
        return ApiWorld()


@unittest.skipUnless(MONGO_URI, 'MONGO_URI not set: the race suite was NOT run against MongoDB')
class MongoRaces(RaceBase, unittest.TestCase):
    def new_world(self):
        return mongo_world()


class RacesAreConfigured(unittest.TestCase):
    def test_mongo_races_are_required_in_ci(self):
        if os.environ.get('REQUIRE_MONGO') == '1':
            self.assertTrue(MONGO_URI, 'REQUIRE_MONGO=1 but MONGO_URI is not set')
        elif not MONGO_URI:
            self.skipTest('MONGO_URI not set: the race suite was NOT run against MongoDB')


if __name__ == '__main__':
    unittest.main()
