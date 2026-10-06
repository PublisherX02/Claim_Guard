"""Shared fixtures for the queue tests: a fake clock, a fake engine whose verdicts a test chooses, a security log in a temp dir,
and stores (the in-memory twin always, MongoDB when MONGO_URI is set)."""
import copy
import os
import sys
import tempfile
import uuid
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import securitylog
from audit_log import ANCHOR_KEY_ENV

KEY = 'k' * 40
RULE_IDS = tuple('R%03d' % i for i in range(1, 16))
MONGO_URI = os.environ.get('MONGO_URI')


class FakeClock:
    def __init__(self, start=1000.0):
        self.t = start

    def __call__(self):
        return self.t

    def advance(self, seconds):
        self.t += seconds
        return self.t


def good_claim(claim_id='C1', patient='P1', date='2026-03-10', **over):
    """A claim that passes the transport validation (the envelope of the public data)."""
    claim = {'schema_version': '1.0.0', 'claim_id': claim_id, 'invoice_number': 'INV-' + claim_id, 'patient_id': patient,
             'member_id': 'M-' + patient, 'provider_id': 'EDU-PROV-01', 'payer_id': 'EDU-PAYER', 'policy_id': 'EDU-PLUS',
             'diagnosis_code': 'DX-EDU-01', 'submission_date': date, 'currency': 'SAR', 'total_amount': 140,
             'coverage': {'coverage_id': 'COV-1', 'status': 'active', 'beneficiary_patient_id': patient, 'member_id': 'M-' + patient,
                          'start_date': '2026-01-01', 'end_date': '2026-12-31'},
             'lines': [{'line_id': 'L1', 'service_code': 'SVC-LAB', 'service_date': date, 'modifier': None, 'quantity': 1,
                        'unit_price': 140, 'net_amount': 140, 'authorization_id': None}],
             'authorizations': [], 'attachments': [], 'notes': 'Synthetic claim.'}
    claim.update(over)
    return claim


def results_of(flags=None, claim_id='C1'):
    """15 results, all PASS; flags maps a rule id to (status, severity)."""
    rows = [{'claim_id': claim_id, 'rule_id': rid, 'status': 'PASS', 'severity': 'high', 'affected_line_ids': [], 'evidence': []}
            for rid in RULE_IDS]
    for rid, (status, severity) in (flags or {}).items():
        for row in rows:
            if row['rule_id'] == rid:
                row.update(status=status, severity=severity)
    return rows


class FakeEngine:
    """engine(claim) -> results. `flags_for` maps a claim id to the flags it should get; unknown claims are clean."""

    def __init__(self, flags_for=None):
        self.flags_for = flags_for or {}
        self.calls = 0

    def __call__(self, claim):
        self.calls += 1
        return results_of(self.flags_for.get(claim.get('claim_id')), claim.get('claim_id'))


class World:
    """A temp security log with its anchor key, and the clock. Use as a context manager or call close()."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.log = securitylog.SecurityLog(Path(self.tmp.name) / 'security_audit.jsonl', KEY)
        self.clock = FakeClock()

    def events(self, kind=None):
        rows = [r['event'] for r in self.log.events(limit=1000)]
        return [e for e in rows if kind is None or e['event_type'] == kind]

    def close(self):
        self.env.stop()
        self.tmp.cleanup()


def store_makers():
    """[(name, factory)] for the stores a test should run against; factory() returns (store, cleanup)."""
    from workqueue.store import MemoryQueueStore
    out = [('memory', lambda: (MemoryQueueStore(), lambda: None))]
    if MONGO_URI:
        from workqueue.store_mongo import MongoQueueStore

        def mongo():
            st = MongoQueueStore(MONGO_URI, 'claimguard_test_' + uuid.uuid4().hex[:12])
            st.ensure_indexes()

            def done():
                st.drop_database()
                st.close()
            return st, done
        out.append(('mongo', mongo))
    return out


def queue_doc(claim_id, version=1, score=4, eligibility='decide_high', patient='P1', lane='A', created=1000.0, input_hash=None):
    """A document for put_triaged with a chosen receipt (no engine needed)."""
    return {'claim_id': claim_id, 'version': version, 'input_hash': input_hash or f'h-{claim_id}-{version}',
            'claim': good_claim(claim_id, patient=patient),
            'results': results_of(None, claim_id),
            'receipt': {'claim_id': claim_id, 'input_hash': input_hash or f'h-{claim_id}-{version}', 'lane': lane,
                        'eligibility': eligibility, 'score': score, 'config_version': 1, 'created_at': created, 'statuses': {}}}


def put_ready(store, claim_id, now=1000.0, **kw):
    """Store a claim and walk it to `ready` at time `now`."""
    version = kw.get('version', 1)
    assert store.put_triaged(queue_doc(claim_id, created=now, **kw))
    assert store.transition(claim_id, version, 'triaged', 'ready', 'system:t', now)
    return store.get(claim_id, version)


def set_cfg(store, **changes):
    """Write the next routing configuration version with the given changes on top of the latest."""
    from workqueue import routing_config as rc
    latest = store.latest_config()
    base = rc.from_doc(latest) if latest else rc.DEFAULT
    import dataclasses
    cfg = rc.validate(dataclasses.replace(base, version=(latest['version'] if latest else 0) + 1, **changes))
    assert store.put_config(rc.to_doc(cfg), latest['version'] if latest else 0)
    return cfg


class FakeModel:
    """A model whose behaviour a test scripts: ok / transient / timeout / fatal / slow / bug, or any other value is returned as-is."""

    def __init__(self, clock):
        from workqueue.breaker import Fatal, Timeout, Transient
        self._exc = {'transient': Transient, 'timeout': Timeout, 'fatal': Fatal, 'bug': KeyError}
        self.clock, self.calls, self.requests, self.script = clock, 0, [], []
        self.text = 'The value {value} on line {line} breaks this rule.'

    def __call__(self, request, deadline):
        self.calls += 1
        self.requests.append((dict(request), deadline))
        step = self.script.pop(0) if self.script else 'ok'
        if step == 'ok':
            return self.text
        if step in self._exc:
            raise self._exc[step]('scripted')
        if step == 'slow':
            self.clock.advance(100)
            return self.text
        return step


def build(store, world, flags_for=None, cfg=None, max_retries=3, guard=None, model=None):
    """A complete queue runtime around `store`: intake, explain step, pipeline steps, dispatcher (no agents) and worker runtime."""
    import random
    from types import SimpleNamespace
    from workqueue import routing_config as rc
    from workqueue.breaker import CircuitBreaker
    from workqueue.dispatcher import Dispatcher
    from workqueue.explain import ExplainStep
    from workqueue.intake import Intake
    from workqueue.worker import Runtime
    ns = SimpleNamespace(flags_for=flags_for if flags_for is not None else {}, sleeps=[], cfg=cfg or rc.DEFAULT)
    ns.engine = FakeEngine(ns.flags_for)
    ns.intake = Intake(store, ns.engine, world.log, world.clock, 'p', 'e')
    ns.model = model or FakeModel(world.clock)
    ns.breaker = CircuitBreaker(world.clock)
    ns.explain = ExplainStep(store, ns.model, guard or (lambda text, result: True), ns.breaker, world.clock, random.Random(5),
                             lambda: ns.cfg, lambda r: f"deterministic {r['rule_id']}", sleep=ns.sleeps.append)
    ns.dispatcher = Dispatcher(store, lambda: [], world.clock)
    ns.rt = Runtime(store, SimpleNamespace(explain=ns.explain), ns.dispatcher, world.clock, max_retries=max_retries, rng=random.Random(1))
    return ns
