"""Settings for the reviewer API, read from the environment and refused when unsafe.

Secrets are never given defaults. `dev=True` generates ephemeral ones for a local demo, is refused off localhost, and is the
only way to use cheap password hashing or insecure cookies.
"""
import secrets
from dataclasses import dataclass, field

from cryptography.fernet import Fernet

LOCAL_HOSTS = frozenset({'127.0.0.1', 'localhost', '::1'})
MIN_SECRET_CHARS = 32


class ConfigError(Exception):
    """The configuration is missing something or unsafe; the server must not start."""


@dataclass(frozen=True)
class Settings:
    jwt_secret: str = field(repr=False)
    fernet_key: str = field(repr=False)
    audit_anchor_key: str = field(repr=False)
    pii_key: str = field(repr=False)
    mongo_uri: str | None = field(repr=False, default=None)
    mongo_db: str = 'claimguard'
    token_ttl_seconds: int = 3600
    bcrypt_rounds: int = 12
    max_failed_logins: int = 5
    lockout_seconds: int = 900
    totp_step: int = 30
    cookie_secure: bool = True
    dev: bool = False


def _secret(env, name, dev):
    value = (env.get(name) or '').strip()
    if not value:
        if dev:
            return secrets.token_urlsafe(48)
        raise ConfigError(f'{name} is required')
    if len(value) < MIN_SECRET_CHARS:
        raise ConfigError(f'{name} must be at least {MIN_SECRET_CHARS} characters')
    return value


def _int(env, name, default, low, high):
    raw = env.get(name)
    if raw is None or str(raw).strip() == '':
        return default
    try:
        value = int(str(raw).strip())
    except ValueError:
        raise ConfigError(f'{name} must be a whole number') from None
    if not low <= value <= high:
        raise ConfigError(f'{name} must be between {low} and {high}')
    return value


def load_settings(env, dev=False, bind_host='127.0.0.1'):
    if dev and bind_host not in LOCAL_HOSTS:
        raise ConfigError('dev mode is only allowed on localhost')
    jwt = _secret(env, 'JWT_SECRET', dev)
    anchor = _secret(env, 'AUDIT_ANCHOR_KEY', dev)
    pii = _secret(env, 'PII_KEY', dev)
    fernet_raw = (env.get('FERNET_KEY') or '').strip()
    if not fernet_raw:
        if not dev:
            raise ConfigError('FERNET_KEY is required')
        fernet_raw = Fernet.generate_key().decode()
    try:
        Fernet(fernet_raw.encode())
    except (ValueError, TypeError):
        raise ConfigError('FERNET_KEY is not a valid Fernet key') from None
    if len({jwt, anchor, pii}) != 3:
        raise ConfigError('JWT_SECRET, AUDIT_ANCHOR_KEY and PII_KEY must be different from one another')
    mongo = (env.get('MONGO_URI') or '').strip() or None
    if mongo is None and not dev:
        raise ConfigError('MONGO_URI is required')
    rounds = _int(env, 'BCRYPT_ROUNDS', 12, 4 if dev else 12, 16)
    secure = True
    if dev and str(env.get('COOKIE_SECURE', 'true')).strip().lower() == 'false':
        secure = False
    return Settings(
        jwt_secret=jwt, fernet_key=fernet_raw, audit_anchor_key=anchor, pii_key=pii, mongo_uri=mongo,
        mongo_db=(env.get('MONGO_DB') or 'claimguard').strip() or 'claimguard',
        token_ttl_seconds=_int(env, 'TOKEN_TTL_SECONDS', 3600, 60, 86400),
        bcrypt_rounds=rounds,
        max_failed_logins=_int(env, 'MAX_FAILED_LOGINS', 5, 1, 20),
        lockout_seconds=_int(env, 'LOCKOUT_SECONDS', 900, 1, 86400),
        cookie_secure=secure, dev=dev)
