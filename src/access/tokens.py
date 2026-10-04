"""Signed session tokens.

A token proves only that this server issued a session to a badge. It carries no level and no permissions: those are read from
the database on every request, so a demotion or deactivation applies at once and a forged claim has nothing to forge.
"""
import secrets
import time
from dataclasses import dataclass

import jwt

ALGORITHM = 'HS256'
REQUIRED = ('sub', 'jti', 'csrf', 'iat', 'exp')
MAX_TOKEN_CHARS = 4096


class TokenError(Exception):
    """The token is missing, malformed, forged, expired or not yet valid."""


@dataclass(frozen=True)
class Issued:
    token: str
    jti: str
    csrf: str
    expires_at: int


def issue(settings, badge_id, now=None):
    issued_at = int(time.time() if now is None else now)
    jti, csrf = secrets.token_urlsafe(16), secrets.token_urlsafe(24)
    expires_at = issued_at + settings.token_ttl_seconds
    token = jwt.encode({'sub': badge_id, 'jti': jti, 'csrf': csrf, 'iat': issued_at, 'exp': expires_at},
                       settings.jwt_secret, algorithm=ALGORITHM)
    return Issued(token, jti, csrf, expires_at)


def decode(settings, token, now=None):
    if not isinstance(token, str) or not token or len(token) > MAX_TOKEN_CHARS:
        raise TokenError('invalid token')
    now = time.time() if now is None else now
    try:
        # Time checks are done below against `now` so they are exact and testable; the algorithm is pinned, so a token
        # claiming "none" or another algorithm is refused before anything else is read.
        payload = jwt.decode(token, settings.jwt_secret, algorithms=[ALGORITHM],
                             options={'require': list(REQUIRED), 'verify_exp': False, 'verify_iat': False,
                                      'verify_nbf': False})
    except (jwt.PyJWTError, ValueError, TypeError, UnicodeError):
        raise TokenError('invalid token') from None
    for name in ('sub', 'jti', 'csrf'):
        if not isinstance(payload.get(name), str) or not payload[name]:
            raise TokenError('invalid token')
    for name in ('iat', 'exp'):
        if type(payload.get(name)) is not int:
            raise TokenError('invalid token')
    if payload['exp'] <= now or payload['iat'] > now:
        raise TokenError('invalid token')
    return payload
