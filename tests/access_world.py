"""A small, fast world for the access tests: in-memory store, cheap hashing, a controllable clock, a real security log.

Not a test module (no test_ prefix): the service, API and attack tests all build on it so they exercise the same wiring.
"""
import os
import sys
import tempfile
import time
from pathlib import Path
from unittest import mock
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import pyotp
from access import config, securitylog, service, store
from audit_log import ANCHOR_KEY_ENV

PASSWORDS = {1: 'Viewer-Pass-1357!', 2: 'Reviewer-Pass-2468!', 3: 'Senior-Pass-3579!', 4: 'Admin-Pass-4680!'}
BADGES = {1: 'CG-1001', 2: 'CG-2002', 3: 'CG-3003', 4: 'CG-4004'}


class Clock:
    def __init__(self, now=1_700_000_000.0):
        self.now = now

    def __call__(self):
        return self.now

    def advance(self, seconds):
        self.now += seconds


class World:
    """Everything a test needs. Call close() (or use as a context manager) to remove the temp files and environment."""

    def __init__(self, store_obj=None, **setting_overrides):
        self._tmp = tempfile.TemporaryDirectory()
        self._env = mock.patch.dict(os.environ, {}, clear=False)
        self._env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        env = {'BCRYPT_ROUNDS': '4', 'COOKIE_SECURE': 'false'}
        env.update({k: str(v) for k, v in setting_overrides.items()})
        self.settings = config.load_settings(env, dev=True)
        self.clock = Clock()
        self.store = store_obj or store.MemoryStore()
        self.log = securitylog.SecurityLog(Path(self._tmp.name) / 'security_audit.jsonl', self.settings.audit_anchor_key)
        self.service = service.AccessService(self.store, self.settings, self.log, clock=self.clock)
        self.secrets = {}                                  # badge -> authenticator secret, as the user's phone would hold it

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def close(self):
        self._env.stop()
        self._tmp.cleanup()

    @property
    def log_path(self):
        return self.log.path

    def log_text(self):
        return self.log.path.read_text(encoding='utf-8') if self.log.path.exists() else ''

    def provision(self, level, badge=None, name=None, **kw):
        badge = badge or BADGES[level]
        user, uri = self.service.provision_user(badge, name or f'User L{level}', kw.pop('password', PASSWORDS[level]), level, **kw)
        self.secrets[badge] = parse_qs(urlparse(uri).query)['secret'][0]
        self.store.update_user(badge, must_change_password=False)
        return user

    def provision_all(self):
        for level in (1, 2, 3, 4):
            self.provision(level)

    def code(self, badge, at=None):
        return pyotp.TOTP(self.secrets[badge]).at(self.clock.now if at is None else at)

    def login(self, level=None, badge=None, password=None, code=None, client='127.0.0.1'):
        badge = badge or BADGES[level]
        password = password if password is not None else PASSWORDS[level]
        return self.service.login(badge, password, code if code is not None else self.code(badge), client)

    def principal(self, level):
        """Log in as the seeded user of this level and return the service's view of the session."""
        self.clock.advance(31)                              # a fresh authenticator step, so repeated calls never replay
        return self.service.authenticate(self.login(level).token)
