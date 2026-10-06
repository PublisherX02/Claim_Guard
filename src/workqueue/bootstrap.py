"""Wiring for the work queue: the store, intake, dispatcher, explanation step and queue service, built the same way for the server, the
admin tool and the tests. Nothing here reads a secret itself; the access stack (settings, user store, security log) is passed in.
"""
import random
import time
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace

from access import config as access_config
from audit_log import AuditLog

from . import explain
from .breaker import CircuitBreaker
from .dispatcher import Dispatcher
from .history import StoreHistory
from .intake import Intake
from .service import QueueService, agents_from_access
from .store import MemoryQueueStore
from .worker import Runtime

ROOT = Path(__file__).resolve().parents[2]


@dataclass
class QueueStack:
    store: object
    dispatcher: Dispatcher
    service: QueueService
    intake: Intake
    explain: explain.ExplainStep
    runtime: Runtime
    engine: object
    review_log: AuditLog


def default_engine(root=ROOT):
    """engine(claim) -> the 15 results, from the real rule engine."""
    from engine_core import config
    from yara_engine import evaluate
    cfg = config(str(root))
    return lambda claim: evaluate(claim, cfg, [])


def open_queue_store(settings, env, dev):
    """MongoDB when MONGO_URI is set, an in-memory store in dev mode only, otherwise a configuration error."""
    if settings.mongo_uri:
        from .store_mongo import MongoQueueStore
        store = MongoQueueStore(settings.mongo_uri, (env.get('QUEUE_DB') or settings.mongo_db + '_queue').strip())
        store.ensure_indexes()
        return store
    if dev:
        return MemoryQueueStore()
    raise access_config.ConfigError('MONGO_URI is required for the work queue')


def build_queue(stack, env=None, dev=False, queue_store=None, engine=None, clock=time.time, model=None, guard=None, rng_seed=None):
    env = env or {}
    store = queue_store or open_queue_store(stack.settings, env, dev)
    review_log = AuditLog(stack.data_dir / 'review_audit.jsonl')
    engine = engine or default_engine()
    dispatcher = Dispatcher(store, agents_from_access(stack.service), clock, rng_seed=rng_seed, securitylog=stack.securitylog)
    service = QueueService(store, dispatcher, stack.service, stack.securitylog, review_log, clock)
    from yara_engine import engine_code_hash, pack_hash
    intake = Intake(store, engine, stack.securitylog, clock, pack_hash(), engine_code_hash(), history=StoreHistory(store))
    step = explain.ExplainStep(store, model or explain.no_model, guard or explain.default_guard, CircuitBreaker(clock), clock,
                               random.Random(), service.routing_config, explain.deterministic_text,
                               model_name=getattr(model, 'name', 'none') if model else 'none')
    runtime = Runtime(store, SimpleNamespace(explain=step), dispatcher, clock, rng=random.Random())
    return QueueStack(store, dispatcher, service, intake, step, runtime, engine, review_log)
