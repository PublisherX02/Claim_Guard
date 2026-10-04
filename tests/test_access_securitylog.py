import json
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import securitylog as sl
from audit import digest
from audit_log import ANCHOR_KEY_ENV

KEY = 'k' * 40


class SecurityLogTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.path = Path(self.tmp.name) / 'security_audit.jsonl'

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def log(self):
        return sl.SecurityLog(self.path, KEY)

    def test_events_are_written_chained_and_verify_strictly_with_a_signed_anchor(self):
        log = self.log()
        log.record('login_success', badge_id='CG-0001', client='127.0.0.1')
        log.record('login_failure', reason='bad_password', badge_attempt='CG-0002', client='127.0.0.1')
        log.record('forbidden', badge_id='CG-0001', method='POST', path='/api/v1/users')
        v = log.verify()
        self.assertTrue(v['ok'], v)
        self.assertEqual((v['events'], v['anchor_signed'], v['key_configured']), (3, True, True))

    def test_a_forged_appended_row_is_detected(self):
        log = self.log()
        log.record('login_success', badge_id='CG-0001', client='x')
        rows = [json.loads(l) for l in self.path.read_text(encoding='utf-8').splitlines() if l.strip()]
        forged = {'sequence': 2, 'recorded_at': 'x', 'previous_hash': rows[-1]['hash'],
                  'event': {'event_type': 'login_success', 'badge_id': 'CG-6666', 'client': 'x'}}
        rows.append({**forged, 'hash': digest(forged)})
        self.path.write_text(''.join(json.dumps(r) + '\n' for r in rows), encoding='utf-8')
        v = log.verify()
        self.assertFalse(v['ok'])
        self.assertIn('unanchored', v['error'])

    def test_a_tampered_event_is_detected(self):
        log = self.log()
        log.record('login_success', badge_id='CG-0001', client='x')
        self.path.write_text(self.path.read_text(encoding='utf-8').replace('CG-0001', 'CG-0002'), encoding='utf-8')
        self.assertFalse(log.verify()['ok'])

    def test_unknown_event_types_are_refused(self):
        log = self.log()
        for bad in ('login', '', None, 5, 'rule_check', 'LOGIN_SUCCESS'):
            with self.assertRaises(ValueError, msg=repr(bad)):
                log.record(bad, badge_id='CG-0001')

    def test_missing_required_fields_are_refused(self):
        with self.assertRaises(ValueError):
            self.log().record('login_success')

    def test_fields_that_could_carry_a_secret_are_refused_whatever_the_case(self):
        log = self.log()
        for name in ('password', 'Password', 'totp', 'totp_code', 'secret', 'client_secret', 'token', 'access_token', 'api_key',
                     'FERNET_KEY', 'password_hash'):
            with self.assertRaises(ValueError, msg=name):
                log.record('login_failure', reason='bad_password', **{name: 'x'})

    def test_forbidden_names_are_refused_inside_nested_values_too(self):
        log = self.log()
        with self.assertRaises(ValueError):
            log.record('user_updated', actor='CG-0001', badge_id='CG-0002', changes={'password_hash': 'x'})
        with self.assertRaises(ValueError):
            log.record('user_updated', actor='CG-0001', badge_id='CG-0002', changes={'a': [{'token': 'x'}]})
        log.record('user_updated', actor='CG-0001', badge_id='CG-0002', changes={'level': [2, 3], 'active': [True, False]})

    def test_values_are_bounded_and_json_safe(self):
        log = self.log()
        log.record('login_failure', reason='bad_password', badge_attempt='x' * 500)
        for bad in ('x' * 501, b'bytes', {1, 2}, object(), float('nan'), float('inf')):
            with self.assertRaises(ValueError, msg=repr(bad)[:30]):
                log.record('login_failure', reason='bad_password', badge_attempt=bad)
        deep = {'a': {'b': {'c': {'d': 1}}}}
        with self.assertRaises(ValueError):
            log.record('user_updated', actor='a', badge_id='b', changes=deep)

    def test_a_short_or_missing_key_is_refused(self):
        for bad in ('', 'short', None, 'x' * 31, ' ' * 40):
            with self.assertRaises(ValueError, msg=repr(bad)):
                sl.SecurityLog(self.path, bad)

    def test_one_key_per_process(self):
        self.log()
        self.log()                                           # the same key again is fine
        with self.assertRaises(ValueError):
            sl.SecurityLog(self.path, 'other-' + 'k' * 40)

    def test_events_are_returned_in_order_with_pagination(self):
        log = self.log()
        for i in range(7):
            log.record('login_success', badge_id=f'CG-000{i}', client='x')
        everything = log.events(limit=100)
        self.assertEqual([e['event']['badge_id'] for e in everything], [f'CG-000{i}' for i in range(7)])
        self.assertEqual([e['sequence'] for e in log.events(limit=3, offset=2)], [3, 4, 5])
        self.assertEqual(log.events(limit=3, offset=99), [])
        for bad in (-1, 0, 10_000_000, 'x', None):
            with self.assertRaises(ValueError, msg=repr(bad)):
                log.events(limit=bad)

    def test_twenty_threads_recording_together_leave_a_valid_complete_chain(self):
        log = self.log()
        barrier = threading.Barrier(20)

        def work(i):
            barrier.wait()
            for j in range(5):
                log.record('login_success', badge_id=f'CG-{i:04d}', client=str(j))
        threads = [threading.Thread(target=work, args=(i,)) for i in range(20)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        v = log.verify()
        self.assertTrue(v['ok'], v)
        self.assertEqual(v['events'], 100)
        self.assertEqual(len(log.events(limit=1000)), 100)


if __name__ == '__main__':
    unittest.main()
