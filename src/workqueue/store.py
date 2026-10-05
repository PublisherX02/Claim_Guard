"""The queue store: an interface, the validation both implementations share, and the in-memory twin.

One claim version is one document, so intake is one atomic write and every state move is one conditional update that also appends
its event. The MongoDB implementation (store_mongo.py) must pass the same contract suite (tests/queue_store_contract.py), which is
how this twin is checked against the real database. Every method refuses non-text identifiers up front: a dict such as
{"$ne": null} must never reach a database query.

Document: {claim_id, version, input_hash, claim, results, receipt, state, state_at, enqueue_pending, lease, events, decided_by,
shadow, explanation, decisions, escalated}. Unique on (claim_id, version) and on (claim_id, input_hash).
"""
import copy
import math
import threading
from typing import Protocol

from access.store import StoreUnavailable, check_text  # noqa: F401 - re-exported: one error type for every store
from . import states

NEW_DOC_KEYS = frozenset({'claim_id', 'version', 'input_hash', 'claim', 'results', 'receipt'})
SETTABLE = frozenset({'lease', 'decided_by', 'shadow', 'explanation', 'enqueue_pending', 'escalated'})
HISTORY_FIELDS = ('claim_id', 'patient_id', 'provider_id', 'submission_date', 'lines', 'authorizations', 'notes',
                  'diagnosis_code', 'attachments')


def check_version(version):
    if type(version) is not int or version < 1:
        raise ValueError('version must be a whole number of 1 or more')
    return version


def check_number(value, what):
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError(f'{what} must be a finite number')
    return value


def check_set_fields(set_fields):
    if set_fields is None:
        return {}
    if type(set_fields) is not dict or set(set_fields) - SETTABLE:
        raise ValueError('a transition may set only: ' + ', '.join(sorted(SETTABLE)))
    return set_fields


def prepare_new_doc(doc):
    """Validate a document handed to put_triaged and return the full stored form (state triaged, outbox marker set)."""
    if type(doc) is not dict or set(doc) != NEW_DOC_KEYS:
        raise ValueError('a new claim document must have exactly: ' + ', '.join(sorted(NEW_DOC_KEYS)))
    check_text(doc['claim_id'], 'claim_id'); check_text(doc['input_hash'], 'input_hash')
    if not doc['claim_id'] or not doc['input_hash']:
        raise ValueError('claim_id and input_hash must not be empty')
    check_version(doc['version'])
    if type(doc['claim']) is not dict or type(doc['receipt']) is not dict or type(doc['results']) is not list:
        raise ValueError('claim and receipt must be objects and results a list')
    receipt = doc['receipt']
    now = check_number(receipt.get('created_at'), 'receipt created_at')
    for key in ('lane', 'eligibility'):
        check_text(receipt.get(key), key)
    full = copy.deepcopy(doc)
    full.update(state='triaged', state_at=now, enqueue_pending=True, lease=None, decided_by=None, shadow=None, explanation=None,
                decisions=[], escalated=False,
                events=[states.make_event('received', 'triaged', 'system:intake', now,
                                          {'lane': receipt['lane'], 'score': receipt.get('score', 0)})])
    return full


def check_decision(decision):
    if type(decision) is not dict or not decision:
        raise ValueError('a decision must be a non-empty object')
    states.check_detail(decision)


def check_config_doc(doc, expected_version):
    if type(doc) is not dict or type(doc.get('version')) is not int:
        raise ValueError('a configuration document needs a whole-number version')
    if type(expected_version) is not int or expected_version < 0 or doc['version'] != expected_version + 1:
        raise ValueError('a configuration document must be exactly the next version')


def check_id_doc(doc, key):
    if type(doc) is not dict or type(doc.get(key)) is not str or not doc[key]:
        raise ValueError(f'the document needs a text {key}')


def project_history(claim):
    return {k: copy.deepcopy(claim.get(k)) for k in HISTORY_FIELDS}


def lane_key(doc):
    return f"{doc['state']}|{doc['receipt']['lane']}|{doc['receipt']['eligibility']}"


class QueueStore(Protocol):
    def put_triaged(self, doc): ...
    def get(self, claim_id, version=None): ...
    def transition(self, claim_id, version, frm, to, actor, now, detail=None, set_fields=None, holder=None): ...
    def add_decision(self, claim_id, version, badge_id, now, decision): ...
    def pending_outbox(self, limit): ...
    def clear_outbox(self, claim_id, version): ...
    def by_state(self, state, limit=1000): ...
    def counts(self): ...
    def lease(self, claim_id, version, badge_id, now, expires_at): ...
    def inbox(self, badge_id): ...
    def heartbeat(self, badge_id, now, expires_at): ...
    def expired(self, now): ...
    def claims_for_patient(self, patient_id): ...
    def put_config(self, doc, expected_version): ...
    def latest_config(self): ...
    def config_history(self): ...
    def append_deal(self, doc): ...
    def get_deal(self, deal_id): ...
    def deals(self, limit=100): ...
    def add_dead_letter(self, doc): ...
    def dead_letters(self, limit=100): ...
    def pop_dead_letter(self, dead_id): ...
    def cache_get(self, key): ...
    def cache_put(self, key, text): ...
    def bump(self, counter, window_key, limit): ...


class MemoryQueueStore:
    """Every method takes one lock, so each behaves like the single atomic database operation it stands in for."""

    def __init__(self):
        self._lock = threading.RLock()
        self._docs = {}            # (claim_id, version) -> document
        self._configs = []
        self._deals = []
        self._dead = {}
        self._cache = {}
        self._counters = {}

    # ---- intake and reads
    def put_triaged(self, doc):
        full = prepare_new_doc(doc)
        with self._lock:
            for d in self._docs.values():
                if d['claim_id'] == full['claim_id'] and (d['input_hash'] == full['input_hash'] or d['version'] == full['version']):
                    return False
            self._docs[(full['claim_id'], full['version'])] = full
            return True

    def get(self, claim_id, version=None):
        check_text(claim_id, 'claim_id')
        with self._lock:
            if version is None:
                versions = [v for (c, v) in self._docs if c == claim_id]
                if not versions:
                    return None
                version = max(versions)
            else:
                check_version(version)
            doc = self._docs.get((claim_id, version))
            return copy.deepcopy(doc) if doc else None

    def transition(self, claim_id, version, frm, to, actor, now, detail=None, set_fields=None, holder=None):
        check_text(claim_id, 'claim_id'); check_version(version)
        check_text(frm, 'state'); check_text(to, 'state'); check_text(actor, 'actor'); check_number(now, 'now')
        if holder is not None:
            check_text(holder, 'badge_id')
        event = states.make_event(frm, to, actor, now, detail)
        fields = check_set_fields(set_fields)
        with self._lock:
            doc = self._docs.get((claim_id, version))
            if doc is None or doc['state'] != frm:
                return None
            if holder is not None and (not doc['lease'] or doc['lease']['badge_id'] != holder):
                return None
            doc['state'], doc['state_at'] = to, now
            doc['events'].append(event)
            for name, value in fields.items():
                doc[name] = copy.deepcopy(value)
            return copy.deepcopy(doc)

    def add_decision(self, claim_id, version, badge_id, now, decision):
        """Record one finding decision, only while the claim is leased to this badge and the lease has not run out."""
        check_text(claim_id, 'claim_id'); check_version(version); check_text(badge_id, 'badge_id'); check_number(now, 'now')
        check_decision(decision)
        with self._lock:
            doc = self._docs.get((claim_id, version))
            if doc is None or doc['state'] != 'leased' or not doc['lease'] or doc['lease']['badge_id'] != badge_id \
                    or doc['lease']['expires_at'] <= now:
                return None
            doc.setdefault('decisions', []).append(copy.deepcopy(decision))
            return copy.deepcopy(doc)

    # ---- outbox
    def pending_outbox(self, limit):
        with self._lock:
            rows = sorted((d for d in self._docs.values() if d['enqueue_pending']), key=lambda d: (d['receipt']['created_at'], d['claim_id']))
            return [copy.deepcopy(d) for d in rows[:limit]]

    def clear_outbox(self, claim_id, version):
        check_text(claim_id, 'claim_id'); check_version(version)
        with self._lock:
            doc = self._docs.get((claim_id, version))
            if doc is None or not doc['enqueue_pending']:
                return False
            doc['enqueue_pending'] = False
            return True

    # ---- queues and leases
    def by_state(self, state, limit=1000):
        check_text(state, 'state')
        with self._lock:
            rows = sorted((d for d in self._docs.values() if d['state'] == state), key=lambda d: (d['state_at'], d['claim_id']))
            return [copy.deepcopy(d) for d in rows[:limit]]

    def counts(self):
        out = {}
        with self._lock:
            for d in self._docs.values():
                out[lane_key(d)] = out.get(lane_key(d), 0) + 1
        return out

    def lease(self, claim_id, version, badge_id, now, expires_at):
        check_text(claim_id, 'claim_id'); check_version(version); check_text(badge_id, 'badge_id')
        check_number(now, 'now'); check_number(expires_at, 'expires_at')
        event = states.make_event('ready', 'leased', badge_id, now, {'expires_at': expires_at})
        with self._lock:
            doc = self._docs.get((claim_id, version))
            if doc is None or doc['state'] != 'ready':
                return None
            doc['state'], doc['state_at'] = 'leased', now
            doc['lease'] = {'badge_id': badge_id, 'leased_at': now, 'expires_at': expires_at, 'heartbeat_at': now}
            doc['events'].append(event)
            return copy.deepcopy(doc)

    def inbox(self, badge_id):
        check_text(badge_id, 'badge_id')
        with self._lock:
            rows = sorted((d for d in self._docs.values() if d['state'] == 'leased' and d['lease']['badge_id'] == badge_id),
                          key=lambda d: (d['lease']['leased_at'], d['claim_id']))
            return [copy.deepcopy(d) for d in rows]

    def heartbeat(self, badge_id, now, expires_at):
        check_text(badge_id, 'badge_id'); check_number(now, 'now'); check_number(expires_at, 'expires_at')
        count = 0
        with self._lock:
            for d in self._docs.values():
                if d['state'] == 'leased' and d['lease']['badge_id'] == badge_id:
                    d['lease']['expires_at'], d['lease']['heartbeat_at'] = expires_at, now
                    count += 1
        return count

    def expired(self, now):
        check_number(now, 'now')
        with self._lock:
            rows = sorted((d for d in self._docs.values() if d['state'] == 'leased' and d['lease']['expires_at'] <= now),
                          key=lambda d: (d['lease']['expires_at'], d['claim_id']))
            return [copy.deepcopy(d) for d in rows]

    # ---- history for the cross-claim rules
    def claims_for_patient(self, patient_id):
        check_text(patient_id, 'patient_id')
        with self._lock:
            latest = {}
            for (cid, ver), d in self._docs.items():
                if cid not in latest or ver > latest[cid]['version']:
                    latest[cid] = d
            return [project_history(d['claim']) for _, d in sorted(latest.items()) if d['claim'].get('patient_id') == patient_id]

    # ---- configuration
    def put_config(self, doc, expected_version):
        check_config_doc(doc, expected_version)
        with self._lock:
            current = self._configs[-1]['version'] if self._configs else 0
            if current != expected_version:
                return False
            self._configs.append(copy.deepcopy(doc))
            return True

    def latest_config(self):
        with self._lock:
            return copy.deepcopy(self._configs[-1]) if self._configs else None

    def config_history(self):
        with self._lock:
            return copy.deepcopy(self._configs)

    # ---- deals, dead letters, cache, counters
    def append_deal(self, doc):
        check_id_doc(doc, 'deal_id')
        with self._lock:
            self._deals.append(copy.deepcopy(doc))

    def get_deal(self, deal_id):
        check_text(deal_id, 'deal_id')
        with self._lock:
            return next((copy.deepcopy(d) for d in self._deals if d['deal_id'] == deal_id), None)

    def deals(self, limit=100):
        with self._lock:
            return copy.deepcopy(list(reversed(self._deals))[:limit])

    def add_dead_letter(self, doc):
        check_id_doc(doc, 'dead_id')
        with self._lock:
            self._dead[doc['dead_id']] = copy.deepcopy(doc)

    def dead_letters(self, limit=100):
        with self._lock:
            return copy.deepcopy(list(self._dead.values())[:limit])

    def pop_dead_letter(self, dead_id):
        check_text(dead_id, 'dead_id')
        with self._lock:
            return self._dead.pop(dead_id, None)

    def cache_get(self, key):
        check_text(key, 'key')
        with self._lock:
            return self._cache.get(key)

    def cache_put(self, key, text):
        check_text(key, 'key'); check_text(text, 'text')
        with self._lock:
            self._cache[key] = text

    def bump(self, counter, window_key, limit):
        check_text(counter, 'counter'); check_text(window_key, 'window_key')
        if type(limit) is not int or limit < 0:
            raise ValueError('limit must be a whole number of 0 or more')
        with self._lock:
            used = self._counters.get((counter, window_key), 0)
            if used >= limit:
                return False
            self._counters[(counter, window_key)] = used + 1
            return True
