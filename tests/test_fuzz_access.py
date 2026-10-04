"""Fuzz the reviewer API: whatever bytes arrive, it must answer with a client error or a plain refusal, never a server error, never a
session without valid credentials, and never a decision recorded under anyone but the logged-in badge."""
import json
import sys
import unittest
from pathlib import Path
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
from access_api_world import ApiWorld
from access_world import BADGES, PASSWORDS
from hypothesis import given, strategies as st

API = '/api/v1'
LEAK_WORDS = ('Traceback', 'File "', 'pydantic', 'starlette', 'fastapi', 'pymongo', 'site-packages')


LEAK = LEAK_WORDS


def assert_sane(test, r, allowed=None):
    test.assertLess(r.status_code, 500, r.text[:200])
    for word in LEAK:
        test.assertNotIn(word, r.text)
    if allowed is not None:
        test.assertIn(r.status_code, allowed)


class LoginFuzz(unittest.TestCase):
    """Its own world: each example moves the clock past the address throttle, which would also expire any logged-in session."""

    @classmethod
    def setUpClass(cls):
        cls.w = ApiWorld()
        cls.w.provision_all()

    @classmethod
    def tearDownClass(cls):
        cls.w.close()

    @given(fs.json_values)
    def test_any_json_value_as_a_login_body_is_refused_and_sets_no_session(self, body):
        self.w.clock.advance(1000)
        r = self.w.client().post(f'{API}/auth/login', content=json.dumps(body), headers={'content-type': 'application/json'})
        assert_sane(self, r, {401, 422, 429})
        self.assertNotIn('set-cookie', r.headers)

    @given(fs.hostile_text, fs.hostile_text, fs.hostile_text)
    def test_any_strings_as_credentials_never_log_in(self, badge, password, code):
        self.w.clock.advance(1000)
        r = self.w.client().post(f'{API}/auth/login', json={'badge_id': badge, 'password': password, 'totp': code})
        assert_sane(self, r, {401, 422, 429})
        self.assertNotIn('set-cookie', r.headers)


class ApiFuzz(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.w = ApiWorld()
        cls.w.provision_all()
        cls.reviewer = cls.w.login_client(2)
        cls.senior = cls.w.login_client(3)
        cls.claim, cls.rule = cls.w.medium()
        cls.review_path = cls.w.review_log.path

    @classmethod
    def tearDownClass(cls):
        cls.w.close()

    def assert_sane(self, r, allowed=None):
        self.assertLess(r.status_code, 500, r.text[:200])
        for word in LEAK_WORDS:
            self.assertNotIn(word, r.text)
        if allowed is not None:
            self.assertIn(r.status_code, allowed)

    @given(fs.json_values)
    def test_any_json_value_as_a_decision_body_is_rejected_or_recorded_under_the_session_badge(self, body):
        before = self.review_path.read_text(encoding='utf-8') if self.review_path.exists() else ''
        r = self.reviewer.post(f'{API}/claims/{self.claim}/findings/{self.rule}/decision', content=json.dumps(body),
                               headers={'content-type': 'application/json', 'X-CSRF-Token': self.reviewer.csrf})
        self.assert_sane(r, {200, 403, 422})
        after = self.review_path.read_text(encoding='utf-8') if self.review_path.exists() else ''
        for line in after[len(before):].split('\n'):
            if line.strip():
                event = json.loads(line)['event']
                self.assertEqual(event.get('actor'), BADGES[2])

    @given(st.text(max_size=80), st.text(max_size=80))
    def test_any_path_parameters_are_a_clean_answer(self, claim, rule):
        # percent-encoded, so the client library accepts the URL and the server still receives the hostile characters
        c, r = quote(claim, safe='', errors='surrogatepass'), quote(rule, safe='', errors='surrogatepass')
        for client in (self.reviewer, self.senior):
            self.assert_sane(client.get(f'{API}/claims/{c}'), {200, 404, 422})
            self.assert_sane(client.post(f'{API}/claims/{c}/findings/{r}/decision', json={'action': 'confirm_issue', 'reason': 'fuzz'}),
                             {200, 404, 403, 422})

    @given(st.dictionaries(st.sampled_from(['limit', 'offset', 'status', 'rule_id', 'x']), fs.hostile_text, max_size=5))
    def test_any_query_parameters_are_a_clean_answer(self, params):
        self.assert_sane(self.reviewer.get(f'{API}/claims', params=params), {200, 422})

    @given(st.text(max_size=200))
    def test_any_session_cookie_value_is_a_401(self, value):
        c = self.w.client()
        try:
            c.cookies.set('cg_session', value, domain='testserver')
        except Exception:                                             # noqa: BLE001 -- the client refused to hold that cookie
            return
        try:
            r = c.get(f'{API}/auth/me')
        except Exception as e:                                        # noqa: BLE001
            if type(e).__module__.split('.')[0] in ('httpx', 'httpx2', 'httpcore', 'httpcore2', 'h11'):
                return
            raise
        self.assert_sane(r, {401})


if __name__ == '__main__':
    unittest.main()
