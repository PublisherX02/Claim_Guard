"""The security audit log of the reviewer API: logins, failures, lockouts, forbidden requests, unmasking, user changes.

It is the same hash-chained, anchored log as the review audit (src/audit_log.py), with two extra rules of its own:
the anchor must be HMAC-signed, and no event may carry anything that looks like a secret. Fields are bounded and JSON-safe, so
a hostile badge or path typed by an attacker can neither bloat the log nor smuggle structure into it.
"""
import json
import math
import os
from pathlib import Path

from audit_log import ANCHOR_KEY_ENV, SECURITY_EVENTS, AuditLog, anchor_status, verify_with_anchor

EVENT_TYPES = frozenset(SECURITY_EVENTS)
FORBIDDEN_NAME_PARTS = ('password', 'passwd', 'totp', 'otp', 'secret', 'token', 'key')
MAX_VALUE_CHARS = 500
MAX_DEPTH = 3
MAX_EVENT_CHARS = 4000
MIN_KEY_CHARS = 32
MAX_PAGE = 1000


def _check_name(name):
    if not isinstance(name, str) or not name:
        raise ValueError('field names must be non-empty text')
    lowered = name.lower()
    if any(part in lowered for part in FORBIDDEN_NAME_PARTS):
        raise ValueError('a field name suggests a secret and may not be logged')


def _check_value(value, depth=0):
    if value is None or isinstance(value, bool):
        return
    if isinstance(value, int):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError('numbers must be finite')
        return
    if isinstance(value, str):
        if len(value) > MAX_VALUE_CHARS:
            raise ValueError(f'text values are limited to {MAX_VALUE_CHARS} characters')
        return
    if isinstance(value, (list, tuple)):
        if depth + 1 > MAX_DEPTH:
            raise ValueError('values are nested too deeply')
        for item in value:
            _check_value(item, depth + 1)
        return
    if isinstance(value, dict):
        if depth + 1 > MAX_DEPTH:
            raise ValueError('values are nested too deeply')
        for key, item in value.items():
            _check_name(key)
            _check_value(item, depth + 1)
        return
    raise ValueError('only text, numbers, booleans, lists and objects can be logged')


class SecurityLog:
    def __init__(self, path, anchor_key):
        if not isinstance(anchor_key, str) or len(anchor_key.strip()) < MIN_KEY_CHARS:
            raise ValueError(f'the anchor key must be at least {MIN_KEY_CHARS} characters')
        current = os.environ.get(ANCHOR_KEY_ENV)
        if current and current != anchor_key:
            raise ValueError('a different anchor key is already set for this process')
        # The audit writer signs anchors with the process-wide key, so this log and the review log share it.
        os.environ[ANCHOR_KEY_ENV] = anchor_key
        self.path = Path(path)
        self._audit = AuditLog(self.path)

    def record(self, event_type, **fields):
        if not isinstance(event_type, str) or event_type not in EVENT_TYPES:
            raise ValueError('unknown security event type')
        for name, value in fields.items():
            _check_name(name)
            _check_value(value)
        event = {'event_type': event_type, **fields}
        if len(json.dumps(event)) > MAX_EVENT_CHARS:
            raise ValueError('event is too large')
        self._audit.append_system_events([event])

    def events(self, limit=100, offset=0):
        if type(limit) is not int or not 1 <= limit <= MAX_PAGE or type(offset) is not int or offset < 0:
            raise ValueError(f'limit must be 1 to {MAX_PAGE} and offset 0 or more')
        if not self.path.exists():
            return []
        rows = []
        for line in self.path.read_text(encoding='utf-8').split('\n'):
            if line.strip():
                row = json.loads(line)
                rows.append({'sequence': row['sequence'], 'recorded_at': row['recorded_at'], 'event': row['event']})
        return rows[offset:offset + limit]

    def verify(self):
        status = anchor_status(self.path)
        if not self.path.exists():
            return {'ok': True, 'events': 0, **status}
        try:
            _head, count = verify_with_anchor(self.path, strict=True)
        except (ValueError, OSError) as e:
            return {'ok': False, 'error': str(e)[:300], **status}
        return {'ok': True, 'events': count, **status}
