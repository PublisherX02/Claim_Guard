"""The claim state machine. Every move is one append-only event, and a move from the wrong state is refused.

    received -> triaged -> explained | explanation_skipped -> ready -> leased -> decided -> rechecked -> triaged
    green claims go triaged -> ready directly; leased -> ready is a lease expiring or being handed back;
    dead_lettered is the side state of a task that failed its retries (an administrator replays it back to triaged).
"""
import math
from types import MappingProxyType

STATES = ('received', 'triaged', 'explained', 'explanation_skipped', 'ready', 'leased', 'decided', 'rechecked', 'dead_lettered')

TRANSITIONS = MappingProxyType({
    'received': frozenset({'triaged', 'dead_lettered'}),
    'triaged': frozenset({'explained', 'explanation_skipped', 'ready', 'dead_lettered'}),
    'explained': frozenset({'ready', 'dead_lettered'}),
    'explanation_skipped': frozenset({'ready', 'dead_lettered'}),
    'ready': frozenset({'leased', 'dead_lettered'}),
    'leased': frozenset({'ready', 'decided', 'dead_lettered'}),
    'decided': frozenset({'rechecked'}),
    'rechecked': frozenset({'triaged'}),
    'dead_lettered': frozenset({'triaged'}),
})
MAX_DETAIL_DEPTH = 3


class IllegalTransition(ValueError):
    """A claim cannot move between these two states."""


def check_transition(frm, to):
    if type(frm) is not str or type(to) is not str or frm not in TRANSITIONS or to not in TRANSITIONS[frm]:
        raise IllegalTransition(f'a claim cannot move from {frm!r} to {to!r}' if type(frm) is str and type(to) is str
                                else 'state names must be text')


def _check_detail(value, depth):
    if isinstance(value, bool) or value is None or type(value) is str:
        return
    if type(value) in (int, float):
        if isinstance(value, float) and not math.isfinite(value):
            raise ValueError('detail numbers must be finite')
        return
    if depth >= MAX_DETAIL_DEPTH:
        raise ValueError('detail is nested too deeply')
    if type(value) is list:
        for item in value:
            _check_detail(item, depth + 1)
        return
    if type(value) is dict:
        for key, item in value.items():
            if type(key) is not str:
                raise ValueError('detail keys must be text')
            _check_detail(item, depth + 1)
        return
    raise ValueError('detail may hold only text, numbers, booleans, lists and objects')


def check_detail(value):
    """Raise ValueError unless value is small plain data (text, numbers, booleans, lists, objects; nested at most 3 deep)."""
    _check_detail(value, 0)


def make_event(frm, to, actor, now, detail=None):
    check_transition(frm, to)
    if type(actor) is not str or not actor:
        raise ValueError('the actor must be a badge or a system worker name')
    if type(now) not in (int, float) or not math.isfinite(now):
        raise ValueError('the time must be a number')
    detail = {} if detail is None else detail
    if type(detail) is not dict:
        raise ValueError('detail must be an object')
    _check_detail(detail, 0)
    return {'from': frm, 'to': to, 'actor': actor, 'at': now, 'detail': detail}
