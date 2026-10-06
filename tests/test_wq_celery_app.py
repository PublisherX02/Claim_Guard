"""The worker entry point: it refuses to start without a safe configuration and, with one, exposes an app whose tasks reach the runtime."""
import importlib
import os
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from audit_log import ANCHOR_KEY_ENV
from cryptography.fernet import Fernet

MONGO_URI = os.environ.get('MONGO_URI')


def good_env(**more):
    env = {'JWT_SECRET': 'j' * 40, 'FERNET_KEY': Fernet.generate_key().decode(), 'AUDIT_ANCHOR_KEY': 'a' * 40, 'PII_KEY': 'p' * 40,
           'MONGO_URI': MONGO_URI or 'mongodb://example.invalid:27017', 'MONGO_DB': 'claimguard_test_' + uuid.uuid4().hex[:10],
           'REDIS_URL': 'redis://127.0.0.1:6379/0'}
    env.update(more)
    return env


class WorkerEntryPointTests(unittest.TestCase):
    def setUp(self):
        sys.modules.pop('workqueue.celery_app', None)
        self.tmp = tempfile.TemporaryDirectory()
        self.saved = {k: os.environ.get(k) for k in ('CLAIMGUARD_DATA_DIR',)}

    def tearDown(self):
        sys.modules.pop('workqueue.celery_app', None)
        self.tmp.cleanup()

    def load(self, env):
        with mock.patch.dict(os.environ, {**env, 'CLAIMGUARD_DATA_DIR': self.tmp.name}, clear=True):
            return importlib.import_module('workqueue.celery_app')

    def test_it_refuses_to_start_without_a_broker(self):
        from access.config import ConfigError
        env = good_env()
        del env['REDIS_URL']
        with self.assertRaises(ConfigError):
            self.load(env)

    def test_it_refuses_to_start_without_the_secrets(self):
        from access.config import ConfigError
        env = good_env()
        del env['JWT_SECRET']
        with self.assertRaises(ConfigError):
            self.load(env)

    @unittest.skipUnless(MONGO_URI, 'MONGO_URI not set: the worker entry point was NOT built against a real database')
    def test_with_a_safe_configuration_it_exposes_an_app_with_every_task_and_the_schedule(self):
        module = self.load(good_env())
        try:
            for name in ('workqueue.process_claim', 'workqueue.deal', 'workqueue.expire', 'workqueue.relay_sweep', 'workqueue.reconcile'):
                self.assertIn(name, module.app.tasks)
            self.assertEqual(set(module.app.conf.beat_schedule), {'deal', 'expire', 'relay', 'reconcile'})
            self.assertIs(module.app.runtime, module.queue.runtime)
            self.assertEqual(module.app.conf.broker_url, 'redis://127.0.0.1:6379/0')
            self.assertFalse(module.app.conf.task_always_eager)
        finally:
            module.queue.store.drop_database()
            module.queue.store.close()


if __name__ == '__main__':
    unittest.main()
