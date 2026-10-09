"""The queue's HTTP routes, installed into the reviewer API by access.api.create_app(..., queue=service).

They live under /api/v1/work (for the people who decide) and /api/v1/queue (for whoever runs the queue). There is deliberately no
route that assigns or moves a particular claim: the only levers are capacity, shifts and the formula's numbers (the configuration),
and a person can only ever act on claims leased to them. Every request body is strict and refuses unknown fields; the actor, the
claim's state and the original status are never taken from the client.
"""
from typing import Any, Literal

from access.api import API, ApiError, DecisionBody, _Body
from fastapi import Depends, Request
from pydantic import Field

from . import feedback
from .service import Conflict


class VerifyBody(_Body):
    action: Literal['verify_clear', 'escalate']


class SubmitBody(_Body):
    claims: list[dict] = Field(min_length=1, max_length=25)


class ConfigBody(_Body):
    expected_version: int = Field(ge=0)
    points: dict[str, dict[str, int]] | None = None
    lane_b_flagged: int | None = None
    lane_b_score: int | None = None
    slice_size: int | None = None
    low_water: int | None = None
    lease_seconds: int | None = None
    aging_per_hour: float | None = None
    ai_per_minute: int | None = None
    ai_daily_budget: int | None = None
    on_shift: list[str] | None = Field(default=None, max_length=1000)
    exclusions: list[list[str]] | None = Field(default=None, max_length=1000)


def install(app, queue, need, principal_dep, run):
    def go(fn, request=None, principal=None):
        """run() from the reviewer API, plus the one error it does not know: a lost lease or stale configuration is a 409."""
        def guarded():
            try:
                return fn()
            except Conflict as e:
                raise ApiError(409, 'conflict', reason=str(e)[:60]) from None
        return run(guarded, request, principal)

    @app.get(f'{API}/work/inbox')
    def inbox(request: Request, principal: Any = Depends(need('claims.decide'))):
        return {'claims': go(lambda: queue.inbox(principal), request, principal)}

    @app.post(f'{API}/work/next')
    def next_claim(request: Request, principal: Any = Depends(need('claims.decide'))):
        return {'claim': go(lambda: queue.next(principal), request, principal)}

    @app.post(f'{API}/work/heartbeat')
    def heartbeat(request: Request, principal: Any = Depends(need('claims.decide'))):
        return {'extended': go(lambda: queue.heartbeat(principal), request, principal)}

    @app.post(API + '/work/claims/{claim_id}/findings/{rule_id}/decision')
    def decide(claim_id: str, rule_id: str, body: DecisionBody, request: Request, principal: Any = Depends(need('claims.decide'))):
        return go(lambda: queue.decide_finding(principal, claim_id, rule_id, body.action, body.reason), request, principal)

    @app.post(API + '/work/claims/{claim_id}/verify')
    def verify(claim_id: str, body: VerifyBody, request: Request, principal: Any = Depends(need('claims.decide'))):
        return go(lambda: queue.decide_green(principal, claim_id, body.action), request, principal)

    @app.get(f'{API}/queue/dashboard')
    def dashboard(request: Request, principal: Any = Depends(need('queue.view'))):
        return go(lambda: queue.dashboard(principal), request, principal)

    @app.get(API + '/ops/trace/{claim_id}')
    def trace(claim_id: str, request: Request, principal: Any = Depends(need('audit.view'))):
        return go(lambda: queue.trace(principal, claim_id), request, principal)

    @app.get(f'{API}/queue/feedback')
    def feedback_report(request: Request, principal: Any = Depends(need('queue.view'))):
        return go(lambda: feedback.report(queue.store), request, principal)

    @app.post(f'{API}/queue/submit')
    def submit(body: SubmitBody, request: Request, principal: Any = Depends(need('routing.manage'))):
        return {'results': go(lambda: queue.submit_claims(principal, body.claims), request, principal)}

    @app.get(f'{API}/queue/config')
    def get_config(request: Request, principal: Any = Depends(need('routing.manage'))):
        return go(lambda: queue.get_config(principal), request, principal)

    @app.patch(f'{API}/queue/config')
    def patch_config(body: ConfigBody, request: Request, principal: Any = Depends(need('routing.manage'))):
        changes = body.model_dump(exclude_unset=True, exclude={'expected_version'})
        if any(v is None for v in changes.values()):
            raise ApiError(422, 'invalid_request')
        new = go(lambda: queue.set_config(principal, changes, body.expected_version), request, principal)
        return {'version': new.version}
