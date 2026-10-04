"""Clearance levels and permission flags.

A level is a default set of flags. A user may additionally hold per-user grants and revokes, but the sensitive flags
(user management and audit access) can only come from level 4, and level 4 deliberately cannot decide claims: whoever
administers the system is not the person who approves its findings (separation of duties).
"""
from types import MappingProxyType

PERMISSIONS = (
    'claims.view', 'claims.view_notes', 'claims.decide', 'claims.decide_high', 'claims.recheck',
    'pii.unmask', 'audit.view', 'audit.verify', 'users.manage',
)
SENSITIVE = frozenset({'users.manage', 'audit.view', 'audit.verify'})

_L1 = frozenset({'claims.view'})
_L2 = _L1 | {'claims.view_notes', 'claims.decide', 'claims.recheck', 'pii.unmask'}
_L3 = _L2 | {'claims.decide_high'}
_L4 = frozenset({'audit.view', 'audit.verify', 'users.manage'})
LEVEL_DEFAULTS = MappingProxyType({1: _L1, 2: _L2, 3: _L3, 4: _L4})
ADMIN_LEVEL = 4


class PermissionDenied(Exception):
    """The actor is not allowed to make this change."""


def _check_level(level):
    if type(level) is not int or level not in LEVEL_DEFAULTS:
        raise ValueError('level must be one of 1, 2, 3, 4')
    return level


def _check_names(names):
    names = tuple(names)
    for n in names:
        if not isinstance(n, str) or n not in PERMISSIONS:
            raise ValueError('unknown permission')
    return names


def effective_permissions(level, grants, revokes):
    """Level defaults, plus grants, minus revokes (a revoke wins over a grant of the same flag)."""
    level = _check_level(level)
    return frozenset((LEVEL_DEFAULTS[level] | set(_check_names(grants))) - set(_check_names(revokes)))


def check_user_change(actor_level, target_level, new_level, grants, revokes, self_change):
    """Raise PermissionDenied (or ValueError for malformed input) unless this actor may make this change."""
    _check_level(actor_level)
    _check_level(target_level)
    _check_level(new_level)
    grants, revokes = _check_names(grants), _check_names(revokes)
    if actor_level != ADMIN_LEVEL:
        raise PermissionDenied('only level 4 may manage users')
    if new_level > actor_level:
        raise PermissionDenied('cannot set a level above your own')
    if self_change and new_level != target_level:
        raise PermissionDenied('cannot change your own level')
    if new_level != ADMIN_LEVEL and SENSITIVE.intersection(grants):
        raise PermissionDenied('user management and audit access can only come from level 4')
