"""The MongoDB queue store.

The guarantees that matter are enforced by the database, not by Python code that could race:
  * a claim version is stored once: unique indexes on (claim_id, version) and on (claim_id, input_hash);
  * every state move and every lease is one find_one_and_update filtered on the expected state, so of 50 racing callers exactly one wins;
  * a configuration version exists once: a unique index on version;
  * a rate counter never passes its limit: one upsert with $inc that only matches while the count is below the limit;
  * cache entries and counters disappear on their own through TTL indexes.
Any driver failure except a duplicate key becomes StoreUnavailable, which callers treat as "refuse".
"""
import copy
from datetime import datetime, timedelta, timezone

import pymongo
from pymongo import ReturnDocument
from pymongo.errors import DuplicateKeyError, PyMongoError

import claim_history
from . import states
from .store import (StoreUnavailable, check_config_doc, check_id_doc, check_number, check_set_fields, check_text, check_version,
                    lane_key, prepare_new_doc, project_history)

NO_ID = {'_id': 0}
CACHE_TTL = timedelta(days=7)
COUNTER_TTL = timedelta(days=3)
BUMP_RETRIES = 5


def _guarded(method):
    def wrapper(self, *args, **kwargs):
        try:
            return method(self, *args, **kwargs)
        except PyMongoError as e:
            if isinstance(e, DuplicateKeyError):
                raise
            raise StoreUnavailable(type(e).__name__) from None
    wrapper.__name__ = method.__name__
    wrapper.__doc__ = method.__doc__
    return wrapper


def _limit(value):
    if type(value) is not int or value < 0:
        raise ValueError('limit must be a whole number of 0 or more')
    return value


def _now():
    return datetime.now(timezone.utc)


class MongoQueueStore:
    def __init__(self, uri, db_name, timeout_ms=3000):
        self._client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=timeout_ms, connectTimeoutMS=timeout_ms,
                                           socketTimeoutMS=max(timeout_ms, 5000), appname='claimguard-queue')
        self._db_name = db_name
        self.raw_db = self._client[db_name]
        self._claims = self.raw_db['claims']
        self._configs = self.raw_db['configs']
        self._deals = self.raw_db['deals']
        self._dead = self.raw_db['dead_letters']
        self._cache = self.raw_db['cache']
        self._counters = self.raw_db['counters']

    def close(self):
        self._client.close()

    @_guarded
    def ensure_indexes(self):
        self._claims.create_index([('claim_id', 1), ('version', 1)], unique=True)
        self._claims.create_index([('claim_id', 1), ('input_hash', 1)], unique=True)
        self._claims.create_index([('state', 1), ('state_at', 1)])
        self._claims.create_index('lease.badge_id')
        self._claims.create_index('enqueue_pending')
        self._claims.create_index('claim.patient_id')
        self._configs.create_index('version', unique=True)
        self._dead.create_index('dead_id', unique=True)
        self._cache.create_index('key', unique=True)
        self._cache.create_index('expires_at', expireAfterSeconds=0)
        self._counters.create_index([('counter', 1), ('window', 1)], unique=True)
        self._counters.create_index('expires_at', expireAfterSeconds=0)

    def drop_database(self):
        self._client.drop_database(self._db_name)

    def ping(self):
        try:
            return bool(self._client.admin.command('ping').get('ok'))
        except PyMongoError:
            return False

    # ---- intake and reads
    @_guarded
    def put_triaged(self, doc):
        full = prepare_new_doc(doc)
        try:
            self._claims.insert_one(full)
        except DuplicateKeyError:
            return False
        return True

    @_guarded
    def get(self, claim_id, version=None):
        check_text(claim_id, 'claim_id')
        if version is None:
            return self._claims.find_one({'claim_id': claim_id}, NO_ID, sort=[('version', -1)])
        check_version(version)
        return self._claims.find_one({'claim_id': claim_id, 'version': version}, NO_ID)

    @_guarded
    def transition(self, claim_id, version, frm, to, actor, now, detail=None, set_fields=None):
        check_text(claim_id, 'claim_id'); check_version(version)
        check_text(frm, 'state'); check_text(to, 'state'); check_text(actor, 'actor'); check_number(now, 'now')
        event = states.make_event(frm, to, actor, now, detail)
        fields = copy.deepcopy(check_set_fields(set_fields))
        return self._claims.find_one_and_update(
            {'claim_id': claim_id, 'version': version, 'state': frm},
            {'$set': {**fields, 'state': to, 'state_at': now}, '$push': {'events': event}},
            projection=NO_ID, return_document=ReturnDocument.AFTER)

    # ---- outbox
    @_guarded
    def pending_outbox(self, limit):
        if _limit(limit) == 0:
            return []
        return list(self._claims.find({'enqueue_pending': True}, NO_ID).sort([('receipt.created_at', 1), ('claim_id', 1)]).limit(limit))

    @_guarded
    def clear_outbox(self, claim_id, version):
        check_text(claim_id, 'claim_id'); check_version(version)
        res = self._claims.update_one({'claim_id': claim_id, 'version': version, 'enqueue_pending': True},
                                      {'$set': {'enqueue_pending': False}})
        return res.modified_count == 1

    # ---- queues and leases
    @_guarded
    def by_state(self, state, limit=1000):
        check_text(state, 'state')
        if _limit(limit) == 0:
            return []
        return list(self._claims.find({'state': state}, NO_ID).sort([('state_at', 1), ('claim_id', 1)]).limit(limit))

    @_guarded
    def counts(self):
        out = {}
        pipeline = [{'$group': {'_id': {'state': '$state', 'lane': '$receipt.lane', 'eligibility': '$receipt.eligibility'},
                                'n': {'$sum': 1}}}]
        for row in self._claims.aggregate(pipeline):
            key = lane_key({'state': row['_id']['state'], 'receipt': {'lane': row['_id']['lane'], 'eligibility': row['_id']['eligibility']}})
            out[key] = row['n']
        return out

    @_guarded
    def lease(self, claim_id, version, badge_id, now, expires_at):
        check_text(claim_id, 'claim_id'); check_version(version); check_text(badge_id, 'badge_id')
        check_number(now, 'now'); check_number(expires_at, 'expires_at')
        event = states.make_event('ready', 'leased', badge_id, now, {'expires_at': expires_at})
        lease = {'badge_id': badge_id, 'leased_at': now, 'expires_at': expires_at, 'heartbeat_at': now}
        return self._claims.find_one_and_update(
            {'claim_id': claim_id, 'version': version, 'state': 'ready'},
            {'$set': {'state': 'leased', 'state_at': now, 'lease': lease}, '$push': {'events': event}},
            projection=NO_ID, return_document=ReturnDocument.AFTER)

    @_guarded
    def inbox(self, badge_id):
        check_text(badge_id, 'badge_id')
        return list(self._claims.find({'state': 'leased', 'lease.badge_id': badge_id}, NO_ID).sort([('lease.leased_at', 1), ('claim_id', 1)]))

    @_guarded
    def heartbeat(self, badge_id, now, expires_at):
        check_text(badge_id, 'badge_id'); check_number(now, 'now'); check_number(expires_at, 'expires_at')
        res = self._claims.update_many({'state': 'leased', 'lease.badge_id': badge_id},
                                       {'$set': {'lease.expires_at': expires_at, 'lease.heartbeat_at': now}})
        return res.matched_count

    @_guarded
    def expired(self, now):
        check_number(now, 'now')
        return list(self._claims.find({'state': 'leased', 'lease.expires_at': {'$lte': now}}, NO_ID).sort([('lease.expires_at', 1), ('claim_id', 1)]))

    # ---- history for the cross-claim rules
    @_guarded
    def claims_for_patient(self, patient_id):
        check_text(patient_id, 'patient_id')
        ids = self._claims.distinct('claim_id', {'claim.patient_id': patient_id})
        if not ids:
            return []
        latest = {}
        for d in self._claims.find({'claim_id': {'$in': ids}}, {'_id': 0, 'claim_id': 1, 'version': 1, 'claim': 1}):
            if d['claim_id'] not in latest or d['version'] > latest[d['claim_id']]['version']:
                latest[d['claim_id']] = d
        return [project_history(d['claim']) for _, d in sorted(latest.items()) if d['claim'].get('patient_id') == patient_id]

    # ---- configuration
    @_guarded
    def put_config(self, doc, expected_version):
        check_config_doc(doc, expected_version)
        top = self._configs.find_one({}, NO_ID, sort=[('version', -1)])
        if (top['version'] if top else 0) != expected_version:
            return False
        try:
            self._configs.insert_one(copy.deepcopy(doc))
        except DuplicateKeyError:
            return False
        return True

    @_guarded
    def latest_config(self):
        return self._configs.find_one({}, NO_ID, sort=[('version', -1)])

    @_guarded
    def config_history(self):
        return list(self._configs.find({}, NO_ID).sort('version', 1))

    # ---- deals, dead letters, cache, counters
    @_guarded
    def append_deal(self, doc):
        check_id_doc(doc, 'deal_id')
        self._deals.insert_one(copy.deepcopy(doc))

    @_guarded
    def get_deal(self, deal_id):
        check_text(deal_id, 'deal_id')
        return self._deals.find_one({'deal_id': deal_id}, NO_ID)

    @_guarded
    def deals(self, limit=100):
        if _limit(limit) == 0:
            return []
        return list(self._deals.find({}, NO_ID).sort('_id', -1).limit(limit))

    @_guarded
    def add_dead_letter(self, doc):
        check_id_doc(doc, 'dead_id')
        self._dead.replace_one({'dead_id': doc['dead_id']}, copy.deepcopy(doc), upsert=True)

    @_guarded
    def dead_letters(self, limit=100):
        if _limit(limit) == 0:
            return []
        return list(self._dead.find({}, NO_ID).sort('_id', 1).limit(limit))

    @_guarded
    def pop_dead_letter(self, dead_id):
        check_text(dead_id, 'dead_id')
        return self._dead.find_one_and_delete({'dead_id': dead_id}, projection=NO_ID)

    @_guarded
    def cache_get(self, key):
        check_text(key, 'key')
        row = self._cache.find_one({'key': key, 'expires_at': {'$gt': _now()}})
        return row['text'] if row else None

    @_guarded
    def cache_put(self, key, text):
        check_text(key, 'key'); check_text(text, 'text')
        self._cache.replace_one({'key': key}, {'key': key, 'text': text, 'expires_at': _now() + CACHE_TTL}, upsert=True)

    @_guarded
    def bump(self, counter, window_key, limit):
        check_text(counter, 'counter'); check_text(window_key, 'window_key')
        if type(limit) is not int or limit < 0:
            raise ValueError('limit must be a whole number of 0 or more')
        if limit == 0:
            return False
        where = {'counter': counter, 'window': window_key}
        for _ in range(BUMP_RETRIES):
            try:
                self._counters.update_one({**where, 'count': {'$lt': limit}},
                                          {'$inc': {'count': 1}, '$setOnInsert': {'expires_at': _now() + COUNTER_TTL}}, upsert=True)
                return True
            except DuplicateKeyError:
                row = self._counters.find_one(where)
                if row is not None and row['count'] >= limit:
                    return False
        raise StoreUnavailable('counter contention')


class MongoHistory:
    """Earlier claims of the same patient from the queue database: the same answer InMemoryHistory gives for the same data."""

    def __init__(self, store):
        self._store = store

    def earlier_claims(self, claim):
        pid = claim.get('patient_id') if isinstance(claim, dict) else None
        if not isinstance(pid, str) or not pid:
            return []
        me, mine = claim_history.order_key(claim), claim.get('claim_id')
        if me is None:
            return []
        rows = [c for c in self._store.claims_for_patient(pid)
                if isinstance(c.get('claim_id'), str) and c['claim_id'] and c['claim_id'] != mine
                and claim_history.order_key(c) is not None and claim_history.order_key(c) < me]
        rows.sort(key=claim_history.order_key)
        return rows
