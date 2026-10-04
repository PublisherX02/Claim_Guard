"""The rules of identity and access. No HTTP in here: the API layer only translates requests to these calls.

Login order (every early exit spends the time of one real password check, so an unknown, locked or inactive account answers no
faster than a real one): shape checks, lookup, locked, inactive, password, authenticator code, consume the code, then issue a
session. Every failure is the same AuthError('invalid_credentials') to the caller; the true reason goes to the security log.
"""
import re
import time
from dataclasses import dataclass

from . import passwords, permissions, tokens, totp
from .store import DuplicateBadge, StoreUnavailable, User

BADGE_RE = re.compile(r'\ACG-\d{4,8}\Z')
MAX_PASSWORD_CHARS = 128
MAX_NAME_CHARS = 100
MAX_CLIENT_CHARS = 64
ADMIN_CHANGEABLE = frozenset({'name', 'level', 'grants', 'revokes', 'active'})
AUDIT_GATE_LIMIT = 200          # failure and rejection events logged per window before they are summarised
AUDIT_GATE_WINDOW = 60


class AuthError(Exception):
    """code is one of 'invalid_credentials', 'token_invalid', 'unavailable'."""

    def __init__(self, code):
        super().__init__('invalid credentials' if code == 'invalid_credentials' else code.replace('_', ' '))
        self.code = code


class Forbidden(Exception):
    """The principal lacks a permission."""


@dataclass(frozen=True)
class Principal:
    user: User
    permissions: frozenset
    jti: str
    csrf: str

    @property
    def badge(self):
        return self.user.badge_id


@dataclass(frozen=True)
class Session:
    token: str
    csrf: str
    expires_at: int
    must_change_password: bool


class _AuditGate:
    """Caps how many failure events one window can add, so a flood of bad logins or bad tokens cannot fill the disk. The first
    event of the next window is preceded by a summary saying how many were left out."""

    def __init__(self, limit, window, clock):
        self.limit, self.window, self.clock, self.state = limit, window, clock, {}

    def allow(self, key):
        now = self.clock()
        start, count, suppressed = self.state.get(key, (now, 0, 0))
        if now - start >= self.window:
            self.state[key] = (now, 1, 0)
            return True, suppressed
        if count < self.limit:
            self.state[key] = (start, count + 1, suppressed)
            return True, 0
        self.state[key] = (start, count, suppressed + 1)
        return False, 0


def _text(value, limit):
    return value[:limit] if isinstance(value, str) else ''


class AccessService:
    def __init__(self, store, settings, securitylog, clock=time.time):
        self.store, self.settings, self.log, self.clock = store, settings, securitylog, clock
        self._gate = _AuditGate(AUDIT_GATE_LIMIT, AUDIT_GATE_WINDOW, clock)
        passwords.dummy_verify('warm-up', settings.bcrypt_rounds)       # build the cached hash before the first real request

    # ---- audit helpers -------------------------------------------------------------------------------------------
    def _gated(self, event_type, reason, **fields):
        allowed, suppressed = self._gate.allow(event_type)
        try:
            if suppressed:
                self.log.record(event_type, reason=f'suppressed_{suppressed}_events')
            if allowed:
                self.log.record(event_type, reason=reason, **fields)
        except OSError:
            pass                                    # the request is already being refused; do not turn that into an error

    def _fail(self, reason, attempt, client):
        self._gated('login_failure', reason, badge_attempt=_text(attempt, 64) if isinstance(attempt, str) else '<non-text>',
                    client=_text(client, MAX_CLIENT_CHARS))
        raise AuthError('invalid_credentials')

    def _count_failure(self, user, now):
        try:
            after = self.store.record_failed_login(user.badge_id, now, self.settings.max_failed_logins, self.settings.lockout_seconds)
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        if after.locked_until is not None and user.locked_until is None:
            try:
                self.log.record('lockout', badge_id=user.badge_id, until=after.locked_until, attempts=after.failed_attempts)
            except OSError:
                pass

    def _must(self, event_type, **fields):
        """Record an event whose loss would hide a privileged action; if it cannot be written the action is refused."""
        try:
            self.log.record(event_type, **fields)
        except OSError:
            raise AuthError('unavailable') from None

    # ---- sessions -----------------------------------------------------------------------------------------------
    def login(self, badge_id, password, totp_code, client=''):
        now = self.clock()
        rounds = self.settings.bcrypt_rounds
        shaped = (isinstance(badge_id, str) and BADGE_RE.match(badge_id) is not None and isinstance(password, str)
                  and 0 < len(password) <= MAX_PASSWORD_CHARS)
        if not shaped:
            passwords.dummy_verify(password if isinstance(password, str) else '', rounds)
            self._fail('malformed', badge_id, client)
        try:
            user = self.store.get_user(badge_id)
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        if user is None:
            passwords.dummy_verify(password, rounds)
            self._fail('unknown_badge', badge_id, client)
        if user.locked_until is not None and user.locked_until > now:
            passwords.dummy_verify(password, rounds)
            self._fail('locked', badge_id, client)
        if not user.active:
            passwords.dummy_verify(password, rounds)
            self._fail('inactive', badge_id, client)
        if not passwords.verify_password(password, user.password_hash):
            self._count_failure(user, now)
            self._fail('bad_password', badge_id, client)
        try:
            secret = totp.decrypt_secret(user.totp_secret_enc, self.settings.fernet_key)
        except ValueError:
            self._fail('seed_unreadable', badge_id, client)
        step = totp.verify_code(secret, totp_code, now, self.settings.totp_step)
        if step is None:
            self._count_failure(user, now)
            self._fail('bad_totp', badge_id, client)
        try:
            fresh = self.store.mark_totp_used(badge_id, step, (step + 2) * self.settings.totp_step)
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        if not fresh:
            self._count_failure(user, now)
            self._fail('replayed_totp', badge_id, client)
        try:
            self.store.record_successful_login(badge_id, now)
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        issued = tokens.issue(self.settings, badge_id, now=now)
        self._must('login_success', badge_id=badge_id, client=_text(client, MAX_CLIENT_CHARS))
        return Session(issued.token, issued.csrf, issued.expires_at, user.must_change_password)

    def _reject(self, reason):
        self._gated('token_rejected', reason)
        raise AuthError('token_invalid')

    def authenticate(self, token):
        now = self.clock()
        try:
            payload = tokens.decode(self.settings, token, now=now)
        except tokens.TokenError:
            self._reject('invalid')
        try:
            if self.store.is_revoked(payload['jti'], now):
                self._reject('revoked')
            user = self.store.get_user(payload['sub'])
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        if user is None or not user.active:
            self._reject('user_gone_or_inactive')
        try:
            perms = permissions.effective_permissions(user.level, user.grants, user.revokes)
        except ValueError:
            self._reject('corrupt_user_record')
        return Principal(user, perms, payload['jti'], payload['csrf'])

    def logout(self, token):
        try:
            payload = tokens.decode(self.settings, token, now=self.clock())
            self.store.revoke_token(payload['jti'], payload['exp'])
        except (tokens.TokenError, StoreUnavailable):
            return
        try:
            self.log.record('logout', badge_id=payload['sub'])
        except OSError:
            pass

    @staticmethod
    def require(principal, permission):
        if permission not in permissions.PERMISSIONS:
            raise ValueError('unknown permission')
        if permission not in principal.permissions:
            raise Forbidden(permission)

    # ---- user management -----------------------------------------------------------------------------------------
    def provision_user(self, badge_id, name, password, level, created_by='system', grants=(), revokes=(), must_change_password=True):
        """Create a user without an acting administrator. For trusted local administration only (scripts/access_admin.py and
        tests); the API never calls this. Returns (user, provisioning URI); the URI carries the authenticator seed and is shown once."""
        if not isinstance(badge_id, str) or BADGE_RE.match(badge_id) is None:
            raise ValueError('badge must look like CG-1234 (4 to 8 digits)')
        if not isinstance(name, str) or not name.strip() or len(name) > MAX_NAME_CHARS:
            raise ValueError('name must be 1 to 100 characters')
        permissions.effective_permissions(level, grants, revokes)                       # validates level and names
        problems = passwords.check_policy(password, badge_id)
        if problems:
            raise ValueError('; '.join(problems))
        secret = totp.new_secret()
        user = User(badge_id=badge_id, name=name.strip(), password_hash=passwords.hash_password(password, self.settings.bcrypt_rounds),
                    totp_secret_enc=totp.encrypt_secret(secret, self.settings.fernet_key), level=level, grants=tuple(grants),
                    revokes=tuple(revokes), active=True, failed_attempts=0, locked_until=None,
                    must_change_password=must_change_password, created_by=created_by, created_at=self.clock(), last_login=None)
        try:
            self.store.create_user(user)
        except DuplicateBadge:
            raise ValueError('that badge already exists') from None
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        return user, totp.provisioning_uri(secret, badge_id)

    def create_user(self, actor, badge_id, name, password, level, grants=(), revokes=()):
        self.require(actor, 'users.manage')
        permissions.check_user_change(actor.user.level, level, level, grants, revokes, self_change=False)
        user, uri = self.provision_user(badge_id, name, password, level, created_by=actor.badge, grants=grants, revokes=revokes)
        self._must('user_created', actor=actor.badge, badge_id=badge_id, level=level)
        return user, uri

    def update_user(self, actor, badge_id, changes):
        self.require(actor, 'users.manage')
        if not isinstance(changes, dict) or not changes:
            raise ValueError('changes must be a non-empty object')
        unknown = set(changes) - ADMIN_CHANGEABLE
        if unknown:
            raise ValueError('these fields cannot be changed: ' + ', '.join(sorted(str(k) for k in unknown)))
        target = self.store.get_user(badge_id)
        if target is None:
            raise KeyError(badge_id)
        new_level = changes.get('level', target.level)
        grants = changes.get('grants', target.grants)
        revokes = changes.get('revokes', target.revokes)
        self_change = actor.badge == badge_id
        permissions.check_user_change(actor.user.level, target.level, new_level, grants, revokes, self_change)
        new_active = changes.get('active', target.active)
        if self_change and new_active is False:
            raise permissions.PermissionDenied('cannot deactivate yourself')
        stops_being_admin = target.level == permissions.ADMIN_LEVEL and target.active and (
            new_level != permissions.ADMIN_LEVEL or new_active is False)
        if stops_being_admin and self.store.count_active_admins() <= 1:
            raise permissions.PermissionDenied('cannot remove the last active administrator')
        updated = self.store.update_user(badge_id, **changes)
        diff = {}
        for field in ('level', 'active'):
            if field in changes:
                diff[field] = [getattr(target, field), getattr(updated, field)]
        for field in ('grants', 'revokes'):
            if field in changes:
                diff[field] = [list(getattr(target, field)), list(getattr(updated, field))]
        if 'name' in changes:
            diff['name'] = ['changed']
        self._must('user_updated', actor=actor.badge, badge_id=badge_id, changes=diff)
        return updated

    def unlock_user(self, actor, badge_id):
        self.require(actor, 'users.manage')
        self.store.update_user(badge_id, failed_attempts=0, locked_until=None)
        self._must('user_unlocked', actor=actor.badge, badge_id=badge_id)

    def reset_totp(self, actor, badge_id):
        self.require(actor, 'users.manage')
        secret = totp.new_secret()
        self.store.update_user(badge_id, totp_secret_enc=totp.encrypt_secret(secret, self.settings.fernet_key))
        self._must('totp_reset', actor=actor.badge, badge_id=badge_id)
        return totp.provisioning_uri(secret, badge_id)

    def change_password(self, principal, old_password, new_password):
        fresh = self.store.get_user(principal.badge)
        if fresh is None or not passwords.verify_password(old_password, fresh.password_hash):
            passwords.dummy_verify(old_password if isinstance(old_password, str) else '', self.settings.bcrypt_rounds)
            self._gated('login_failure', 'bad_old_password', badge_attempt=principal.badge)
            raise AuthError('invalid_credentials')
        problems = passwords.check_policy(new_password, principal.badge)
        if problems:
            raise ValueError('; '.join(problems))
        if new_password == old_password:
            raise ValueError('the new password must be different')
        self.store.update_user(principal.badge, password_hash=passwords.hash_password(new_password, self.settings.bcrypt_rounds),
                               must_change_password=False)
        self.store.revoke_token(principal.jti, self.clock() + self.settings.token_ttl_seconds)
        self._must('password_changed', badge_id=principal.badge)
