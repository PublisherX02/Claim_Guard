"""Wiring: settings, store, logs, service and (for the server) the claim store and the app, built the same way for the admin
tool, the server and the tests.

Dev mode keeps its generated secrets in <data dir>/dev_secrets.json (git-ignored) so a second run can still verify the audit log
the first run signed. That file exists only for local demos; production reads every secret from the environment.
"""
import json
import os
import secrets
from dataclasses import dataclass
from pathlib import Path

from cryptography.fernet import Fernet

from . import api, config, claimstore
from .securitylog import SecurityLog
from .service import AccessService
from .store import MemoryStore

ROOT = Path(__file__).resolve().parents[2]
SECRET_NAMES = ('JWT_SECRET', 'AUDIT_ANCHOR_KEY', 'PII_KEY', 'FERNET_KEY')


@dataclass
class Stack:
    settings: config.Settings
    store: object
    securitylog: SecurityLog
    service: AccessService
    data_dir: Path
    queue: object = None


def _data_dir(env, data_dir):
    directory = Path(data_dir or env.get('CLAIMGUARD_DATA_DIR') or 'instance')
    directory.mkdir(parents=True, exist_ok=True)
    return directory


def _with_dev_secrets(env, directory):
    """Fill in any secret the environment does not provide from a persisted dev file, creating it on first use."""
    env = dict(env)
    path = directory / 'dev_secrets.json'
    saved = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
    changed = False
    for name in SECRET_NAMES:
        if not (env.get(name) or '').strip():
            if name not in saved:
                saved[name] = Fernet.generate_key().decode() if name == 'FERNET_KEY' else secrets.token_urlsafe(48)
                changed = True
            env[name] = saved[name]
    if changed:
        path.write_text(json.dumps(saved), encoding='utf-8')
        try:
            os.chmod(path, 0o600)
        except OSError:
            pass
    return env


def build_stack(env, dev=False, store=None, data_dir=None, bind_host='127.0.0.1', clock=None):
    directory = _data_dir(env, data_dir)
    if dev:
        env = _with_dev_secrets(env, directory)
    settings = config.load_settings(env, dev=dev, bind_host=bind_host)
    if store is None:
        if settings.mongo_uri:
            from .store_mongo import MongoStore
            store = MongoStore(settings.mongo_uri, settings.mongo_db)
            store.ensure_indexes()
        elif dev:
            store = MemoryStore()
        else:
            raise config.ConfigError('MONGO_URI is required')
    log = SecurityLog(directory / 'security_audit.jsonl', settings.audit_anchor_key)
    return Stack(settings, store, log, AccessService(store, settings, log, **({'clock': clock} if clock else {})), directory)


def build_app(env, dev=False, store=None, data_dir=None, claims_path=None, bind_host='127.0.0.1', queue=False, queue_store=None):
    from audit_log import AuditLog
    from engine_core import config as engine_config

    stack = build_stack(env, dev=dev, store=store, data_dir=data_dir, bind_host=bind_host)
    review_log = AuditLog(stack.data_dir / 'review_audit.jsonl')
    path = claims_path or env.get('CLAIMS_PATH') or ROOT / 'data' / 'development' / 'claims.jsonl'
    claims = claimstore.FileClaimStore.from_jsonl(path, engine_config(ROOT))
    service = None
    if queue:
        from workqueue import bootstrap as queue_bootstrap
        stack.queue = queue_bootstrap.build_queue(stack, env, dev=dev, queue_store=queue_store)
        service = stack.queue.service
    app = api.create_app(stack.service, claims, review_log, stack.securitylog, stack.settings, queue=service, ui=True)
    return app, stack
