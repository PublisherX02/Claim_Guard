"""The reviewer HTTP API (FastAPI). It only translates requests into AccessService calls and shapes the answers.

Rules this layer enforces on every request:
  * a session cookie (HttpOnly, SameSite=Strict) authenticates; every unsafe method must also echo the CSRF token that is signed
    into that session;
  * permissions are checked per endpoint and what a caller may not see is absent from the response, not flagged;
  * the reviewer recorded on a decision is the session's badge, never anything the client sends;
  * bodies are small, JSON only, strictly typed with unknown fields refused, and errors never echo input or internals.
"""
import hmac
import json
import logging
import time
from datetime import datetime, timezone
from typing import Any, Literal

import review_workflow
from audit_log import verify_with_anchor
from fastapi import Depends, FastAPI, Query, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel, ConfigDict, Field
from starlette.exceptions import HTTPException as StarletteHTTPException

from . import masking, permissions
from .ui import UI_CSP, UI_PREFIX
from .service import AuthError, DuplicateUser, Forbidden

API = '/api/v1'
SESSION_COOKIE = 'cg_session'
CSRF_COOKIE = 'cg_csrf'
MAX_BODY_BYTES = 65536
SAFE_METHODS = frozenset({'GET', 'HEAD', 'OPTIONS'})
PASSWORD_CHANGE_PATHS = frozenset({f'{API}/auth/me', f'{API}/auth/logout', f'{API}/auth/change-password'})
ACTIONS = ('confirm_issue', 'dismiss_with_reason', 'request_information', 'mark_corrected_for_recheck')
LOGIN_FAILURE_LIMIT = 20
LOGIN_FAILURE_WINDOW = 900
THROTTLE_MAX_CLIENTS = 10_000
log = logging.getLogger('claimguard.access')

_SECURITY_HEADERS = [(b'cache-control', b'no-store'), (b'x-content-type-options', b'nosniff'), (b'x-frame-options', b'DENY'),
                     (b'referrer-policy', b'no-referrer'), (b'content-security-policy', b"default-src 'none'")]


# ---- request and response models ------------------------------------------------------------------------------------
class _Body(BaseModel):
    model_config = ConfigDict(extra='forbid', strict=True)


class LoginBody(_Body):
    badge_id: str = Field(max_length=64)
    password: str = Field(max_length=128)
    totp: str = Field(max_length=12)


class ChangePasswordBody(_Body):
    old_password: str = Field(max_length=128)
    new_password: str = Field(max_length=128)


class DecisionBody(_Body):
    action: Literal['confirm_issue', 'dismiss_with_reason', 'request_information', 'mark_corrected_for_recheck']
    reason: str = Field(min_length=1, max_length=1000)


class UnmaskBody(_Body):
    reason: str = Field(min_length=1, max_length=500)


class RecheckBody(_Body):
    rule_ids: list[str] = Field(min_length=1, max_length=15)


class CreateUserBody(_Body):
    badge_id: str = Field(max_length=64)
    name: str = Field(max_length=200)
    password: str = Field(max_length=128)
    level: int
    grants: list[str] = Field(default_factory=list, max_length=20)
    revokes: list[str] = Field(default_factory=list, max_length=20)


class UpdateUserBody(_Body):
    name: str | None = Field(default=None, max_length=200)
    level: int | None = None
    grants: list[str] | None = Field(default=None, max_length=20)
    revokes: list[str] | None = Field(default=None, max_length=20)
    active: bool | None = None


class ApiError(Exception):
    def __init__(self, status, code, headers=None, **extra):
        super().__init__(code)
        self.status, self.code, self.headers, self.extra = status, code, headers, extra


def _json(status, code, headers=None, **extra):
    return JSONResponse({'error': code, **extra}, status_code=status, headers=headers)


# ---- ASGI layers ------------------------------------------------------------------------------------------------------
class SecurityHeaders:
    """Adds the same hardening headers to every response, including errors raised by the layers below."""

    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope['type'] != 'http':
            return await self.app(scope, receive, send)

        async def send_with_headers(message):
            if message['type'] == 'http.response.start':
                names = {n for n, _ in _SECURITY_HEADERS}
                headers = _SECURITY_HEADERS
                if scope.get('path', '').startswith(UI_PREFIX):          # the console's own files: scripts and styles from this origin only
                    headers = [(n, UI_CSP.encode() if n == b'content-security-policy' else v) for n, v in headers]
                message = {**message, 'headers': [h for h in message.get('headers', []) if h[0].lower() not in names] + headers}
            await send(message)
        await self.app(scope, receive, send_with_headers)


class RequestGuard:
    """Reads at most MAX_BODY_BYTES before the application sees anything, insists on JSON for bodies, and turns any unexpected
    exception into a plain 500 with no details."""

    def __init__(self, app, max_bytes=MAX_BODY_BYTES):
        self.app, self.max_bytes = app, max_bytes

    @staticmethod
    async def _reply(send, status, code):
        body = json.dumps({'error': code}).encode()
        await send({'type': 'http.response.start', 'status': status,
                    'headers': [(b'content-type', b'application/json'), (b'content-length', str(len(body)).encode())]})
        await send({'type': 'http.response.body', 'body': body})

    async def __call__(self, scope, receive, send):
        if scope['type'] != 'http':
            return await self.app(scope, receive, send)
        headers = {k.lower(): v for k, v in scope['headers']}
        length = headers.get(b'content-length')
        if length is not None and not length.isdigit():
            return await self._reply(send, 400, 'bad_request')
        if length is not None and int(length) > self.max_bytes:
            return await self._reply(send, 413, 'payload_too_large')
        if scope['method'] in ('POST', 'PUT', 'PATCH'):
            has_body = (length is not None and length != b'0') or b'transfer-encoding' in headers
            if has_body and headers.get(b'content-type', b'').split(b';')[0].strip().lower() != b'application/json':
                return await self._reply(send, 415, 'unsupported_media_type')
        body = b''
        while True:
            message = await receive()
            if message['type'] == 'http.disconnect':
                return
            body += message.get('body', b'')
            if len(body) > self.max_bytes:
                return await self._reply(send, 413, 'payload_too_large')
            if not message.get('more_body', False):
                break
        replayed = False

        async def replay():
            nonlocal replayed
            if not replayed:
                replayed = True
                return {'type': 'http.request', 'body': body, 'more_body': False}
            return await receive()
        started = False

        async def tracking_send(message):
            nonlocal started
            if message['type'] == 'http.response.start':
                started = True
            await send(message)
        try:
            await self.app(scope, replay, tracking_send)
        except Exception as e:                     # noqa: BLE001 -- the one place unexpected errors are contained
            log.error('unhandled %s on %s %s', type(e).__name__, scope['method'], scope['path'][:100])
            if not started:
                await self._reply(send, 500, 'server_error')


class _LoginThrottle:
    """Failed logins per client address; in-process, so it protects a single server process (documented limit)."""

    def __init__(self, clock, limit=LOGIN_FAILURE_LIMIT, window=LOGIN_FAILURE_WINDOW):
        self.clock, self.limit, self.window, self.failures = clock, limit, window, {}

    def _recent(self, client):
        cutoff = self.clock() - self.window
        recent = [t for t in self.failures.get(client, ()) if t > cutoff]
        if recent:
            self.failures[client] = recent
        else:
            self.failures.pop(client, None)
        return recent

    def blocked(self, client):
        return len(self._recent(client)) >= self.limit

    def record_failure(self, client):
        recent = self._recent(client)
        recent.append(self.clock())
        self.failures[client] = recent
        if len(self.failures) > THROTTLE_MAX_CLIENTS:                 # never let the table itself become the memory problem
            for stale in list(self.failures)[:len(self.failures) - THROTTLE_MAX_CLIENTS]:
                del self.failures[stale]


def _client_ip(request):
    return request.client.host if request.client else 'unknown'


# ---- the application --------------------------------------------------------------------------------------------------
def create_app(service, claims, review_log, securitylog, settings, clock=time.time, queue=None, ui=False):
    app = FastAPI(title='ClaimGuard reviewer API', docs_url=None, redoc_url=None, openapi_url=None)
    app.add_middleware(RequestGuard)
    app.add_middleware(SecurityHeaders)
    throttle = _LoginThrottle(clock)

    @app.exception_handler(ApiError)
    async def _api_error(request, exc):
        return _json(exc.status, exc.code, exc.headers, **exc.extra)

    @app.exception_handler(RequestValidationError)
    async def _invalid(request, exc):
        if request.url.path == f'{API}/auth/login':
            throttle.record_failure(_client_ip(request))
        return _json(422, 'invalid_request')                           # never echo the input: it may hold a password

    @app.exception_handler(StarletteHTTPException)
    async def _http(request, exc):
        names = {404: 'not_found', 405: 'method_not_allowed'}
        return _json(exc.status_code, names.get(exc.status_code, f'http_{exc.status_code}'))

    # ---- dependencies
    def principal_dep(request: Request):
        token = request.cookies.get(SESSION_COOKIE)
        if not token:
            raise ApiError(401, 'unauthenticated')
        try:
            principal = service.authenticate(token)
        except AuthError as e:
            raise (ApiError(503, 'unavailable') if e.code == 'unavailable' else ApiError(401, 'unauthenticated')) from None
        if request.method not in SAFE_METHODS:
            sent = request.headers.get('x-csrf-token', '')
            if not hmac.compare_digest(sent.encode('utf-8', 'replace'), principal.csrf.encode()):
                raise ApiError(403, 'csrf')
        if principal.user.must_change_password and request.url.path not in PASSWORD_CHANGE_PATHS:
            raise ApiError(403, 'password_change_required')
        return principal

    def forbid(request, principal):
        service.note_forbidden(principal.badge, request.method, request.url.path)
        raise ApiError(403, 'forbidden')

    def need(permission):
        def dependency(request: Request, principal: Any = Depends(principal_dep)):
            if permission not in principal.permissions:
                forbid(request, principal)
            return principal
        return dependency

    def run(fn, request=None, principal=None):
        """Call a service method and translate its errors to HTTP."""
        try:
            return fn()
        except Forbidden:
            forbid(request, principal)
        except permissions.PermissionDenied:
            forbid(request, principal)
        except DuplicateUser:
            raise ApiError(409, 'conflict') from None
        except KeyError:
            raise ApiError(404, 'not_found') from None
        except (ValueError, TypeError) as e:
            raise ApiError(422, 'invalid_request', detail=str(e)[:300]) from None
        except AuthError as e:
            raise (ApiError(503, 'unavailable') if e.code == 'unavailable' else ApiError(401, 'invalid_credentials')) from None

    # ---- health and authentication
    @app.get('/healthz')
    def healthz():
        healthy = service.store.ping()
        return JSONResponse({'status': 'ok' if healthy else 'degraded'}, status_code=200 if healthy else 503)

    @app.post(f'{API}/auth/login')
    def login(body: LoginBody, request: Request):
        client = _client_ip(request)
        if throttle.blocked(client):
            return _json(429, 'too_many_attempts', headers={'Retry-After': str(LOGIN_FAILURE_WINDOW)})
        try:
            session = service.login(body.badge_id, body.password, body.totp, client=client)
        except AuthError as e:
            if e.code == 'unavailable':
                return _json(503, 'unavailable')
            throttle.record_failure(client)
            return _json(401, 'invalid_credentials')
        response = JSONResponse({'expires_at': session.expires_at, 'csrf': session.csrf, 'must_change_password': session.must_change_password})
        for name, value, http_only in ((SESSION_COOKIE, session.token, True), (CSRF_COOKIE, session.csrf, False)):
            response.set_cookie(name, value, max_age=settings.token_ttl_seconds, httponly=http_only, secure=settings.cookie_secure,
                                samesite='strict', path='/')
        return response

    @app.post(f'{API}/auth/logout')
    def logout(request: Request, principal: Any = Depends(principal_dep)):
        service.logout(request.cookies.get(SESSION_COOKIE))
        response = JSONResponse({'status': 'logged_out'})
        for name in (SESSION_COOKIE, CSRF_COOKIE):
            response.delete_cookie(name, path='/')
        return response

    @app.get(f'{API}/auth/me')
    def me(principal: Any = Depends(principal_dep)):
        u = principal.user
        return {'badge_id': u.badge_id, 'name': u.name, 'level': u.level, 'permissions': sorted(principal.permissions),
                'must_change_password': u.must_change_password}

    @app.post(f'{API}/auth/change-password')
    def change_password(body: ChangePasswordBody, request: Request, principal: Any = Depends(principal_dep)):
        run(lambda: service.change_password(principal, body.old_password, body.new_password), request, principal)
        return {'status': 'changed'}

    # ---- claims
    def finding_actions(principal, finding):
        if finding['status'] not in review_workflow.REVIEWABLE or 'claims.decide' not in principal.permissions:
            return None
        if finding['severity'] == 'high' and 'claims.decide_high' not in principal.permissions:
            return None
        return list(ACTIONS)

    def claim_view(principal, claim, results, unmasked=False):
        shaped = masking.shape_claim(claim, results, principal.permissions, settings.pii_key, unmasked=unmasked)
        state = review_workflow.review_state(results, review_log.path)
        by_rule = {r['rule_id']: r for r in results}
        for finding in shaped['results']:
            key = (claim['claim_id'], finding['rule_id'])
            if key in state:
                finding['review_state'] = state[key]
            actions = finding_actions(principal, by_rule[finding['rule_id']])
            if actions:
                finding['allowed_actions'] = actions
        view = {'claim': shaped['claim'], 'findings': shaped['results']}
        claim_actions = [a for a, perm in (('unmask', 'pii.unmask'), ('recheck', 'claims.recheck')) if perm in principal.permissions]
        if claim_actions:
            view['allowed_actions'] = claim_actions
        return view

    @app.get(f'{API}/claims')
    def list_claims(limit: int = Query(50, ge=1, le=200), offset: int = Query(0, ge=0), status: str | None = Query(None, max_length=20),
                    rule_id: str | None = Query(None, max_length=10), principal: Any = Depends(need('claims.view'))):
        try:
            return {'claims': claims.summaries(limit=limit, offset=offset, status=status, rule_id=rule_id)}
        except ValueError:
            raise ApiError(422, 'invalid_request') from None

    @app.get(API + '/claims/{claim_id}')
    def get_claim(claim_id: str, principal: Any = Depends(need('claims.view'))):
        pair = claims.get(claim_id)
        if pair is None:
            raise ApiError(404, 'not_found')
        return claim_view(principal, *pair)

    @app.post(API + '/claims/{claim_id}/unmask')
    def unmask(claim_id: str, body: UnmaskBody, request: Request, principal: Any = Depends(need('pii.unmask'))):
        pair = claims.get(claim_id)
        if pair is None:
            raise ApiError(404, 'not_found')
        run(lambda: service.record('unmask', badge_id=principal.badge, claim_id=claim_id[:64], reason=body.reason), request, principal)
        return claim_view(principal, *pair, unmasked=True)

    @app.post(API + '/claims/{claim_id}/findings/{rule_id}/decision')
    def decide(claim_id: str, rule_id: str, body: DecisionBody, request: Request, principal: Any = Depends(need('claims.decide'))):
        pair = claims.get(claim_id)
        if pair is None:
            raise ApiError(404, 'not_found')
        _, results = pair
        finding = next((r for r in results if r['rule_id'] == rule_id), None)
        if finding is None:
            raise ApiError(404, 'not_found')
        if finding['severity'] == 'high' and 'claims.decide_high' not in principal.permissions:
            forbid(request, principal)
        decision = {'claim_id': claim_id, 'rule_id': rule_id, 'action': body.action, 'actor': principal.badge, 'reason': body.reason,
                    'created_at': datetime.fromtimestamp(clock(), tz=timezone.utc).isoformat(), 'original_status': finding['status']}
        try:
            review_workflow.validate_decision(decision, review_workflow.index_findings(results))
            review_log.append_review_decisions([decision])
        except review_workflow.DecisionError as e:
            raise ApiError(422, 'decision_rejected', detail=str(e)[:300]) from None
        except (ValueError, OSError):
            raise ApiError(422, 'decision_rejected') from None
        run(lambda: service.record('decision', badge_id=principal.badge, claim_id=claim_id, rule_id=rule_id, action=body.action),
            request, principal)
        return {'recorded': True, 'review_state': review_workflow.review_state(results, review_log.path).get((claim_id, rule_id))}

    @app.post(API + '/claims/{claim_id}/recheck')
    def recheck(claim_id: str, body: RecheckBody, principal: Any = Depends(need('claims.recheck'))):
        # A real recheck needs prior run traces and stored result versions, which arrive when claims move into the database
        # (the task-queue work). The permission is enforced now so the endpoint's contract is fixed.
        raise ApiError(501, 'not_implemented')

    # ---- audit
    @app.get(f'{API}/audit/events')
    def audit_events(request: Request, limit: int = Query(100, ge=1, le=1000), offset: int = Query(0, ge=0),
                     principal: Any = Depends(need('audit.view'))):
        run(lambda: service.record('audit_read', badge_id=principal.badge), request, principal)
        return {'events': securitylog.events(limit=limit, offset=offset)}

    @app.get(f'{API}/audit/verify')
    def audit_verify(request: Request, principal: Any = Depends(need('audit.verify'))):
        security = securitylog.verify()
        try:
            if not review_log.path.exists():                       # nothing decided yet: an empty log is intact, not a failure
                review = {'ok': True, 'events': 0}
            else:
                _, count = verify_with_anchor(review_log.path, strict=False)
                review = {'ok': True, 'events': count}
        except (ValueError, OSError) as e:
            review = {'ok': False, 'error': str(e)[:300]}
        run(lambda: service.record('audit_verify', badge_id=principal.badge, ok=bool(security['ok'] and review['ok'])), request, principal)
        return {'security': security, 'review': review}

    # ---- users
    @app.get(f'{API}/users')
    def users_list(request: Request, principal: Any = Depends(need('users.manage'))):
        users = run(lambda: service.list_users(principal), request, principal)
        now = clock()
        return {'users': [{'badge_id': u.badge_id, 'name': u.name, 'level': u.level, 'active': u.active,
                           'locked': bool(u.locked_until and u.locked_until > now), 'must_change_password': u.must_change_password,
                           'last_login': u.last_login} for u in users]}

    @app.post(f'{API}/users', status_code=201)
    def users_create(body: CreateUserBody, request: Request, principal: Any = Depends(need('users.manage'))):
        user, uri = run(lambda: service.create_user(principal, body.badge_id, body.name, body.password, body.level,
                                                    tuple(body.grants), tuple(body.revokes)), request, principal)
        return {'badge_id': user.badge_id, 'provisioning_uri': uri}

    @app.patch(API + '/users/{badge_id}')
    def users_patch(badge_id: str, body: UpdateUserBody, request: Request, principal: Any = Depends(need('users.manage'))):
        changes = body.model_dump(exclude_unset=True)
        if any(v is None for v in changes.values()):
            raise ApiError(422, 'invalid_request')
        updated = run(lambda: service.update_user(principal, badge_id, changes), request, principal)
        return {'badge_id': updated.badge_id, 'level': updated.level, 'active': updated.active}

    @app.post(API + '/users/{badge_id}/unlock')
    def users_unlock(badge_id: str, request: Request, principal: Any = Depends(need('users.manage'))):
        run(lambda: service.unlock_user(principal, badge_id), request, principal)
        return {'status': 'unlocked'}

    @app.post(API + '/users/{badge_id}/reset-totp')
    def users_reset_totp(badge_id: str, request: Request, principal: Any = Depends(need('users.manage'))):
        return {'provisioning_uri': run(lambda: service.reset_totp(principal, badge_id), request, principal)}

    if ui:
        from . import ui as console
        console.install(app)

    if queue is not None:
        from workqueue import api as queue_api
        queue_api.install(app, queue, need, principal_dep, run)

    return app
