"""The MongoDB user store.

The guarantees that matter are enforced by the database, not by Python code that could race:
  * one user per badge: a unique index on users.badge_id;
  * a one-time code is accepted once: a unique index on (badge_id, step) in used_totp, so of 50 parallel submissions
    exactly one insert succeeds;
  * failed-login counting and locking happen in a single atomic update pipeline, so parallel failures lose no count;
  * used codes and revoked tokens disappear on their own through TTL indexes.
Any driver failure except a duplicate key becomes StoreUnavailable, which callers treat as "refuse".
"""
from datetime import datetime, timezone

import pymongo
from pymongo import ReturnDocument
from pymongo.errors import DuplicateKeyError, PyMongoError

from .store import (DuplicateBadge, StoreUnavailable, UPDATABLE, User, check_text, validate_field, validate_user)


def _dt(ts):
    return datetime.fromtimestamp(ts, tz=timezone.utc)


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


def _to_user(doc):
    return User(
        badge_id=doc['badge_id'], name=doc['name'], password_hash=doc['password_hash'], totp_secret_enc=doc['totp_secret_enc'],
        level=doc['level'], grants=tuple(doc.get('grants', ())), revokes=tuple(doc.get('revokes', ())), active=doc['active'],
        failed_attempts=doc['failed_attempts'], locked_until=doc.get('locked_until'),
        must_change_password=doc['must_change_password'], created_by=doc['created_by'], created_at=doc['created_at'],
        last_login=doc.get('last_login'))


class MongoStore:
    def __init__(self, uri, db_name, timeout_ms=3000):
        self._client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=timeout_ms, connectTimeoutMS=timeout_ms,
                                           socketTimeoutMS=max(timeout_ms, 5000), appname='claimguard-access')
        self._db_name = db_name
        self.raw_db = self._client[db_name]
        self._users = self.raw_db['users']
        self._totp = self.raw_db['used_totp']
        self._revoked = self.raw_db['revoked_tokens']

    def close(self):
        self._client.close()

    @_guarded
    def ensure_indexes(self):
        self._users.create_index('badge_id', unique=True)
        self._totp.create_index([('badge_id', 1), ('step', 1)], unique=True)
        self._totp.create_index('expires_at', expireAfterSeconds=0)
        self._revoked.create_index('jti', unique=True)
        self._revoked.create_index('expires_at', expireAfterSeconds=0)

    def drop_database(self):
        self._client.drop_database(self._db_name)

    def ping(self):
        try:
            return bool(self._client.admin.command('ping').get('ok'))
        except PyMongoError:
            return False

    @_guarded
    def create_user(self, user):
        validate_user(user)
        doc = {'badge_id': user.badge_id, 'name': user.name, 'password_hash': user.password_hash,
               'totp_secret_enc': user.totp_secret_enc, 'level': user.level, 'grants': list(user.grants),
               'revokes': list(user.revokes), 'active': user.active, 'failed_attempts': user.failed_attempts,
               'locked_until': user.locked_until, 'must_change_password': user.must_change_password,
               'created_by': user.created_by, 'created_at': user.created_at, 'last_login': user.last_login}
        try:
            self._users.insert_one(doc)
        except DuplicateKeyError:
            raise DuplicateBadge(user.badge_id) from None

    @_guarded
    def get_user(self, badge_id):
        check_text(badge_id)
        doc = self._users.find_one({'badge_id': badge_id})
        return _to_user(doc) if doc else None

    @_guarded
    def list_users(self):
        return [_to_user(d) for d in self._users.find({}).sort('badge_id', 1)]

    @_guarded
    def update_user(self, badge_id, /, **fields):
        check_text(badge_id)
        unknown = set(fields) - UPDATABLE
        if unknown:
            raise ValueError(f'cannot update: {", ".join(sorted(unknown))}')
        for name, value in fields.items():
            validate_field(name, value)
        changes = {n: (list(v) if n in ('grants', 'revokes') else v) for n, v in fields.items()}
        doc = self._users.find_one_and_update({'badge_id': badge_id}, {'$set': changes}, return_document=ReturnDocument.AFTER) \
            if changes else self._users.find_one({'badge_id': badge_id})
        if doc is None:
            raise KeyError(badge_id)
        return _to_user(doc)

    @_guarded
    def record_failed_login(self, badge_id, now, max_failed, lockout_seconds):
        check_text(badge_id)
        # One atomic pipeline update: a lock that has run out starts a new count; otherwise the count goes up by one; the
        # lock is set when the count reaches the limit and is never extended by further failures.
        expired = {'$and': [{'$ne': ['$locked_until', None]}, {'$lte': ['$locked_until', now]}]}
        pipeline = [
            {'$set': {'failed_attempts': {'$cond': [expired, 1, {'$add': ['$failed_attempts', 1]}]},
                      'locked_until': {'$cond': [expired, None, '$locked_until']}}},
            {'$set': {'locked_until': {'$cond': [{'$and': [{'$gte': ['$failed_attempts', max_failed]},
                                                           {'$eq': ['$locked_until', None]}]},
                                                 now + lockout_seconds, '$locked_until']}}},
        ]
        doc = self._users.find_one_and_update({'badge_id': badge_id}, pipeline, return_document=ReturnDocument.AFTER)
        if doc is None:
            raise KeyError(badge_id)
        return _to_user(doc)

    @_guarded
    def record_successful_login(self, badge_id, now):
        check_text(badge_id)
        result = self._users.update_one({'badge_id': badge_id},
                                        {'$set': {'failed_attempts': 0, 'locked_until': None, 'last_login': now}})
        if result.matched_count == 0:
            raise KeyError(badge_id)

    @_guarded
    def mark_totp_used(self, badge_id, step, expires_at):
        check_text(badge_id)
        if type(step) is not int:
            raise TypeError('step must be an integer')
        try:
            self._totp.insert_one({'badge_id': badge_id, 'step': step, 'expires_at': _dt(expires_at)})
        except DuplicateKeyError:
            return False
        return True

    @_guarded
    def revoke_token(self, jti, expires_at):
        check_text(jti, 'jti')
        update = {'$max': {'expires_ts': float(expires_at), 'expires_at': _dt(expires_at)}}
        for _ in range(2):                         # two simultaneous first revocations race on the unique index; retry once
            try:
                self._revoked.update_one({'jti': jti}, update, upsert=True)
                return
            except DuplicateKeyError:
                continue

    @_guarded
    def is_revoked(self, jti, now):
        check_text(jti, 'jti')
        return self._revoked.find_one({'jti': jti, 'expires_ts': {'$gt': float(now)}}) is not None

    @_guarded
    def count_active_admins(self):
        return self._users.count_documents({'level': 4, 'active': True})
