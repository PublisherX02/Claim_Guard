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
