"""The user store: an interface, and an in-memory implementation used by fast tests and the demo.

The MongoDB implementation (store_mongo.py) must pass the same contract suite (tests/access_store_contract.py), so this twin is
checked against the real database's behaviour. Every method refuses non-string identifiers up front: a dict such as
{"$ne": null} must never reach a database query.
"""
import threading
from dataclasses import dataclass, replace
from typing import Protocol

UPDATABLE = frozenset({'name', 'password_hash', 'totp_secret_enc', 'level', 'grants', 'revokes', 'active',
                       'must_change_password', 'locked_until', 'failed_attempts'})


class DuplicateBadge(Exception):
    """A user with this badge already exists."""


class StoreUnavailable(Exception):
    """The database cannot be reached or failed; callers must fail closed."""


@dataclass
class User:
    badge_id: str
    name: str
    password_hash: str
    totp_secret_enc: str
    level: int
    grants: tuple = ()
    revokes: tuple = ()
    active: bool = True
    failed_attempts: int = 0
    locked_until: float | None = None
    must_change_password: bool = False
    created_by: str = ''
    created_at: float = 0.0
    last_login: float | None = None


def check_text(value, what='identifier'):
    if type(value) is not str:
        raise TypeError(f'{what} must be a string')
    return value


def _is_number(v):
    return type(v) in (int, float)


def _text_list(v):
    return isinstance(v, (tuple, list)) and all(type(x) is str for x in v)


_FIELD_CHECKS = {
    'badge_id': lambda v: type(v) is str and bool(v),
    'name': lambda v: type(v) is str,
    'password_hash': lambda v: type(v) is str,
    'totp_secret_enc': lambda v: type(v) is str,
    'level': lambda v: type(v) is int and v in (1, 2, 3, 4),
    'grants': _text_list,
    'revokes': _text_list,
    'active': lambda v: type(v) is bool,
    'failed_attempts': lambda v: type(v) is int and v >= 0,
    'locked_until': lambda v: v is None or _is_number(v),
    'must_change_password': lambda v: type(v) is bool,
    'created_by': lambda v: type(v) is str,
    'created_at': _is_number,
    'last_login': lambda v: v is None or _is_number(v),
}


def validate_field(name, value):
    """Every stored field has exactly one acceptable type; anything else (a dict, a list, a number where text belongs) is
    refused before it can reach a database query or document."""
    check = _FIELD_CHECKS.get(name)
    if check is None or not check(value):
        raise TypeError(f'{name} has the wrong type')


def validate_user(user):
    if not isinstance(user, User):
        raise TypeError('user must be a User')
    for name in _FIELD_CHECKS:
        validate_field(name, getattr(user, name))


def copy_user(user):
    return replace(user, grants=tuple(user.grants), revokes=tuple(user.revokes))


class UserStore(Protocol):
    def ping(self) -> bool: ...
    def create_user(self, user: User) -> None: ...
    def get_user(self, badge_id: str) -> User | None: ...
    def list_users(self) -> list: ...
    def update_user(self, badge_id: str, /, **fields) -> User: ...
    def record_failed_login(self, badge_id: str, now: float, max_failed: int, lockout_seconds: int) -> User: ...
    def record_successful_login(self, badge_id: str, now: float) -> None: ...
    def mark_totp_used(self, badge_id: str, step: int, expires_at: float) -> bool: ...
    def revoke_token(self, jti: str, expires_at: float) -> None: ...
    def is_revoked(self, jti: str, now: float) -> bool: ...
    def count_active_admins(self) -> int: ...


class MemoryStore:
    def __init__(self):
        self._lock = threading.RLock()
        self._users = {}
        self._totp = {}
        self._revoked = {}

    def ping(self):
        return True

    def create_user(self, user):
        validate_user(user)
        with self._lock:
            if user.badge_id in self._users:
                raise DuplicateBadge(user.badge_id)
            self._users[user.badge_id] = copy_user(user)

    def get_user(self, badge_id):
        check_text(badge_id)
        with self._lock:
            user = self._users.get(badge_id)
            return copy_user(user) if user else None

    def list_users(self):
        with self._lock:
            return [copy_user(u) for u in self._users.values()]

    def update_user(self, badge_id, /, **fields):
        check_text(badge_id)
        unknown = set(fields) - UPDATABLE
        if unknown:
            raise ValueError(f'cannot update: {", ".join(sorted(unknown))}')
        for name, value in fields.items():
            validate_field(name, value)
        with self._lock:
            user = self._users.get(badge_id)
            if user is None:
                raise KeyError(badge_id)
            for name, value in fields.items():
                setattr(user, name, tuple(value) if name in ('grants', 'revokes') else value)
            return copy_user(user)

    def record_failed_login(self, badge_id, now, max_failed, lockout_seconds):
        check_text(badge_id)
        with self._lock:
            user = self._users.get(badge_id)
            if user is None:
                raise KeyError(badge_id)
            if user.locked_until is not None and user.locked_until <= now:      # the lock ran out: start a new count
                user.failed_attempts, user.locked_until = 1, None
            else:
                user.failed_attempts += 1
            if user.failed_attempts >= max_failed and user.locked_until is None:  # a lock is never extended by more failures
                user.locked_until = now + lockout_seconds
            return copy_user(user)

    def record_successful_login(self, badge_id, now):
        check_text(badge_id)
        with self._lock:
            user = self._users.get(badge_id)
            if user is None:
                raise KeyError(badge_id)
            user.failed_attempts, user.locked_until, user.last_login = 0, None, now

    def mark_totp_used(self, badge_id, step, expires_at):
        check_text(badge_id)
        if type(step) is not int:
            raise TypeError('step must be an integer')
        with self._lock:
            key = (badge_id, step)
            if key in self._totp:
                return False
            self._totp[key] = expires_at
            return True

    def revoke_token(self, jti, expires_at):
        check_text(jti, 'jti')
        with self._lock:
            self._revoked[jti] = max(expires_at, self._revoked.get(jti, 0))

    def is_revoked(self, jti, now):
        check_text(jti, 'jti')
        with self._lock:
            expires = self._revoked.get(jti)
            return expires is not None and expires > now

    def count_active_admins(self):
        with self._lock:
            return sum(1 for u in self._users.values() if u.level == 4 and u.active)
