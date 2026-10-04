import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'tests'))
from unittest import mock

import access_admin
import serve_access
from access import bootstrap, passwords, service, store, totp
from audit_log import ANCHOR_KEY_ENV
from cryptography.fernet import Fernet

GOOD_ENV = {'JWT_SECRET': 'j' * 40, 'FERNET_KEY': Fernet.generate_key().decode(), 'AUDIT_ANCHOR_KEY': 'a' * 40, 'PII_KEY': 'p' * 40,
            'MONGO_URI': 'mongodb://example.invalid:27017', 'BCRYPT_ROUNDS': '12'}
DEV_ENV = {'BCRYPT_ROUNDS': '4'}


class Cli:
    """Runs access_admin.main in an isolated environment with a shared in-memory store and a scripted password prompt."""

    def __init__(self, passwords_to_type=()):
        self.tmp = tempfile.TemporaryDirectory()
        self.store = store.MemoryStore()
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.typed = list(passwords_to_type)

    def close(self):
        self.env.stop()
        self.tmp.cleanup()

    def run(self, *argv, typed=None):
        if typed is not None:
            self.typed = list(typed)
        out = io.StringIO()

        def prompt(_message=''):
            return self.typed.pop(0)
        code = access_admin.main(list(argv), env=dict(DEV_ENV), getpass_fn=prompt, out=out, store=self.store, data_dir=self.tmp.name)
        return code, out.getvalue()


class AdminCliTests(unittest.TestCase):
    def setUp(self):
        self.cli = Cli()

    def tearDown(self):
        self.cli.close()

    def test_create_admin_makes_a_level_four_user_with_hashed_password_and_encrypted_seed(self):
        code, out = self.cli.run('--dev', 'create-admin', '--badge', 'CG-4004', '--name', 'Ada Admin', typed=['Admin-Pass-4680!', 'Admin-Pass-4680!'])
        self.assertEqual(code, 0, out)
        u = self.cli.store.get_user('CG-4004')
        self.assertEqual((u.level, u.active, u.must_change_password), (4, True, False))
        self.assertTrue(passwords.verify_password('Admin-Pass-4680!', u.password_hash))
        self.assertNotIn('Admin-Pass-4680!', out)
        uri = next(line for line in out.splitlines() if line.startswith('otpauth://'))
        secret = parse_qs(urlparse(uri).query)['secret'][0]
        self.assertNotEqual(u.totp_secret_enc, secret)
        self.assertEqual(out.count('otpauth://'), 1)

    def test_weak_mismatched_and_duplicate_inputs_are_refused_cleanly(self):
        for typed, argv, needle in ((['short', 'short'], ('--badge', 'CG-4004'), 'password'),
                                    (['Admin-Pass-4680!', 'Admin-Pass-4681!'], ('--badge', 'CG-4004'), 'match'),
                                    (['Admin-Pass-4680!', 'Admin-Pass-4680!'], ('--badge', 'nope'), 'badge')):
            code, out = self.cli.run('--dev', 'create-admin', *argv, '--name', 'X', typed=typed)
            self.assertEqual(code, 2, (typed, out))
            self.assertIn(needle, out.lower())
            self.assertNotIn('Traceback', out)
        self.assertEqual(self.cli.store.list_users(), [])
        self.cli.run('--dev', 'create-admin', '--badge', 'CG-4004', '--name', 'X', typed=['Admin-Pass-4680!', 'Admin-Pass-4680!'])
        code, out = self.cli.run('--dev', 'create-admin', '--badge', 'CG-4004', '--name', 'Y', typed=['Admin-Pass-4680!', 'Admin-Pass-4680!'])
        self.assertEqual(code, 2)
        self.assertIn('already exists', out)

    def test_seed_demo_needs_dev_mode_and_creates_one_user_per_level(self):
        code, out = self.cli.run('seed-demo')
        self.assertEqual(code, 2)
        self.assertIn('--dev', out)
        self.assertEqual(self.cli.store.list_users(), [])
        code, out = self.cli.run('--dev', 'seed-demo')
        self.assertEqual(code, 0, out)
        users = {u.level: u for u in self.cli.store.list_users()}
        self.assertEqual(sorted(users), [1, 2, 3, 4])
        self.assertEqual(out.count('otpauth://'), 4)
        for level, u in users.items():
            line = next(l for l in out.splitlines() if u.badge_id in l and 'password' in l.lower())
            printed = line.split('password:')[1].strip()
            self.assertTrue(passwords.verify_password(printed, u.password_hash))
            self.assertEqual(passwords.check_policy(printed, u.badge_id), [])
        code, out = self.cli.run('--dev', 'seed-demo')
        self.assertEqual(code, 2)
        self.assertEqual(len(self.cli.store.list_users()), 4)

    def test_list_unlock_and_reset_totp(self):
        self.cli.run('--dev', 'seed-demo')
        code, out = self.cli.run('--dev', 'list')
        self.assertEqual(code, 0)
        self.assertEqual(sum(1 for l in out.splitlines() if l.startswith('CG-')), 4)
        for forbidden in ('hash', '$2b$', 'otpauth', 'gAAAA'):
            self.assertNotIn(forbidden, out)
        for _ in range(5):
            self.cli.store.record_failed_login('CG-2002', 1_700_000_000.0, 5, 900)
        self.assertEqual(self.cli.run('--dev', 'unlock', '--badge', 'CG-2002')[0], 0)
        self.assertIsNone(self.cli.store.get_user('CG-2002').locked_until)
        before = self.cli.store.get_user('CG-2002').totp_secret_enc
        code, out = self.cli.run('--dev', 'reset-totp', '--badge', 'CG-2002')
        self.assertEqual(code, 0)
        self.assertEqual(out.count('otpauth://'), 1)
        self.assertNotEqual(self.cli.store.get_user('CG-2002').totp_secret_enc, before)
        for sub in ('unlock', 'reset-totp'):
            code, out = self.cli.run('--dev', sub, '--badge', 'CG-9999')
            self.assertEqual(code, 2)
            self.assertIn('no such user', out)

    def test_admin_actions_land_in_the_security_log(self):
        self.cli.run('--dev', 'create-admin', '--badge', 'CG-4004', '--name', 'X', typed=['Admin-Pass-4680!', 'Admin-Pass-4680!'])
        text = (Path(self.cli.tmp.name) / 'security_audit.jsonl').read_text(encoding='utf-8')
        self.assertIn('user_created', text)
        self.assertNotIn('Admin-Pass-4680!', text)

    def test_nothing_secret_is_printed(self):
        code, out = self.cli.run('--dev', 'seed-demo')
        self.assertEqual(code, 0)
        for line in out.splitlines():
            self.assertNotIn('JWT_SECRET', line)
            self.assertNotIn('FERNET_KEY', line)


class BootstrapAndServeTests(unittest.TestCase):
    def setUp(self):
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def test_dev_mode_builds_a_working_app(self):
        from fastapi.testclient import TestClient
        app, stack = bootstrap.build_app(dict(DEV_ENV), dev=True, data_dir=self.tmp.name,
                                         claims_path=ROOT / 'data' / 'stress' / 'claims.jsonl')
        client = TestClient(app)
        self.assertEqual(client.get('/healthz').json(), {'status': 'ok'})
        self.assertEqual(client.get('/api/v1/claims').status_code, 401)
        self.assertTrue(stack.settings.dev)

    def test_serve_refuses_dev_mode_off_localhost_without_a_traceback(self):
        out = io.StringIO()
        code = serve_access.main(['--dev', '--host', '0.0.0.0'], env=dict(DEV_ENV), out=out, run=lambda *a, **k: self.fail('must not start'))
        self.assertEqual(code, 2)
        self.assertIn('localhost', out.getvalue())
        self.assertNotIn('Traceback', out.getvalue())

    def test_serve_refuses_missing_or_weak_secrets_without_echoing_them(self):
        for env in ({}, {'JWT_SECRET': 'tiny-but-secret'}):
            out = io.StringIO()
            code = serve_access.main([], env=env, out=out, run=lambda *a, **k: self.fail('must not start'))
            self.assertEqual(code, 2)
            self.assertNotIn('tiny-but-secret', out.getvalue())
            self.assertNotIn('Traceback', out.getvalue())

    def test_serve_refuses_a_public_bind_without_tls(self):
        out = io.StringIO()
        code = serve_access.main(['--host', '0.0.0.0'], env=dict(GOOD_ENV), out=out, run=lambda *a, **k: self.fail('must not start'),
                                 store=store.MemoryStore(), data_dir=self.tmp.name, claims_path=ROOT / 'data' / 'stress' / 'claims.jsonl')
        self.assertEqual(code, 2)
        self.assertIn('TLS', out.getvalue())

    def test_serve_starts_with_safe_server_options(self):
        calls = []
        out = io.StringIO()
        code = serve_access.main(['--dev', '--port', '9443'], env=dict(DEV_ENV), out=out, run=lambda app, **kw: calls.append(kw),
                                 data_dir=self.tmp.name, claims_path=ROOT / 'data' / 'stress' / 'claims.jsonl')
        self.assertEqual(code, 0, out.getvalue())
        self.assertEqual(calls[0]['host'], '127.0.0.1')
        self.assertEqual(calls[0]['port'], 9443)
        self.assertFalse(calls[0]['server_header'])
        self.assertIn('DEV MODE', out.getvalue())


if __name__ == '__main__':
    unittest.main()
