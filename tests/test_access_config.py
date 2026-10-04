import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import config as c
from cryptography.fernet import Fernet

GOOD = {
    'JWT_SECRET': 'j' * 40,
    'FERNET_KEY': Fernet.generate_key().decode(),
    'AUDIT_ANCHOR_KEY': 'a' * 40,
    'PII_KEY': 'p' * 40,
    'MONGO_URI': 'mongodb://example.invalid:27017',
}


def env(**over):
    e = dict(GOOD)
    e.update(over)
    return {k: v for k, v in e.items() if v is not None}


class LoadSettingsTests(unittest.TestCase):
    def test_a_complete_environment_loads_with_production_defaults(self):
        s = c.load_settings(env())
        self.assertEqual((s.bcrypt_rounds, s.max_failed_logins, s.lockout_seconds, s.totp_step), (12, 5, 900, 30))
        self.assertTrue(s.cookie_secure)
        self.assertFalse(s.dev)

    def test_each_missing_secret_is_refused(self):
        for name in ('JWT_SECRET', 'FERNET_KEY', 'AUDIT_ANCHOR_KEY', 'PII_KEY', 'MONGO_URI'):
            with self.assertRaises(c.ConfigError, msg=name):
                c.load_settings(env(**{name: None}))

    def test_short_or_blank_secrets_are_refused(self):
        for name in ('JWT_SECRET', 'AUDIT_ANCHOR_KEY', 'PII_KEY'):
            for value in ('', 'short', 'x' * 31, '   ' * 20):
                with self.assertRaises(c.ConfigError, msg=f'{name}={value!r}'):
                    c.load_settings(env(**{name: value}))

    def test_an_invalid_fernet_key_is_refused(self):
        for value in ('not-a-key', '', 'a' * 44):
            with self.assertRaises(c.ConfigError, msg=value):
                c.load_settings(env(FERNET_KEY=value))

    def test_the_three_secrets_must_differ(self):
        with self.assertRaises(c.ConfigError):
            c.load_settings(env(PII_KEY='j' * 40))

    def test_overrides_are_validated(self):
        s = c.load_settings(env(BCRYPT_ROUNDS='13', TOKEN_TTL_SECONDS='600'))
        self.assertEqual((s.bcrypt_rounds, s.token_ttl_seconds), (13, 600))
        for name, value in (('BCRYPT_ROUNDS', '11'), ('BCRYPT_ROUNDS', 'x'), ('TOKEN_TTL_SECONDS', '0'),
                            ('TOKEN_TTL_SECONDS', '999999'), ('MAX_FAILED_LOGINS', '0')):
            with self.assertRaises(c.ConfigError, msg=f'{name}={value}'):
                c.load_settings(env(**{name: value}))

    def test_repr_never_shows_a_secret(self):
        e = env()
        text = repr(c.load_settings(e)) + str(c.load_settings(e))
        for name in ('JWT_SECRET', 'FERNET_KEY', 'AUDIT_ANCHOR_KEY', 'PII_KEY'):
            self.assertNotIn(e[name], text, name)


class DevModeTests(unittest.TestCase):
    def test_dev_generates_valid_distinct_ephemeral_keys(self):
        a, b = c.load_settings({}, dev=True), c.load_settings({}, dev=True)
        self.assertNotEqual(a.jwt_secret, b.jwt_secret)
        self.assertNotEqual(a.fernet_key, b.fernet_key)
        Fernet(a.fernet_key.encode())
        self.assertEqual(len({a.jwt_secret, a.audit_anchor_key, a.pii_key}), 3)
        self.assertTrue(a.dev)

    def test_dev_allows_cheap_hashing_but_not_below_four(self):
        self.assertEqual(c.load_settings({'BCRYPT_ROUNDS': '4'}, dev=True).bcrypt_rounds, 4)
        with self.assertRaises(c.ConfigError):
            c.load_settings({'BCRYPT_ROUNDS': '3'}, dev=True)

    def test_dev_is_refused_off_localhost(self):
        for host in ('0.0.0.0', '10.0.0.5', 'example.com', '::'):
            with self.assertRaises(c.ConfigError, msg=host):
                c.load_settings({}, dev=True, bind_host=host)
        for host in ('127.0.0.1', 'localhost', '::1'):
            c.load_settings({}, dev=True, bind_host=host)

    def test_production_never_allows_insecure_cookies(self):
        self.assertTrue(c.load_settings(env(COOKIE_SECURE='false')).cookie_secure)
        self.assertFalse(c.load_settings({'COOKIE_SECURE': 'false'}, dev=True).cookie_secure)


if __name__ == '__main__':
    unittest.main()
