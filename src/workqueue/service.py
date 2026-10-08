"""The queue service: what an agent or an administrator may do with the queue. No HTTP in here.

Rules enforced in this file, not in the web layer:
  * a person decides only claims leased to them, and only while the lease runs; permissions are read from the user store again at
    the moment of a decision (what the session token said when the request began is not trusted);
  * a decision is written to the claim by one conditional database update that checks the lease holder and its expiry, so a decision
    that loses a race with an expiry or a re-deal is refused and writes nothing;
  * nobody assigns a particular claim to a particular person: the administrator changes capacity, shifts and the formula's numbers
    through the versioned configuration, and the dispatcher deals;
  * level 4 holds queue.view and routing.manage but not claims.decide, so whoever runs the queue cannot decide its claims;
  * a claim with a high-severity finding needs two different seniors: the first resolves every finding and the claim waits for a
    countersignature; a second senior (never the first) decides the high-severity findings again without seeing the first's answer.
    Agreeing decides the claim; disagreeing sends it, escalated, to a third senior whose decision is final.
"""
import dataclasses
import hashlib
import json
from datetime import datetime, timezone

import review_workflow
from access import masking, permissions
from access.service import AuthError, Forbidden
from access.store import StoreUnavailable

from . import routing_config as rc
from . import triage
from .dispatcher import Agent

REVIEWABLE = review_workflow.REVIEWABLE
RESOLVING = review_workflow.RESOLVING
GREEN_ACTIONS = ('verify_clear', 'escalate')
CONFIG_FIELDS = frozenset(f.name for f in dataclasses.fields(rc.RoutingConfig)) - {'version'}
LOG_VALUE_CHARS = 400
MAX_SUBMIT = 25
ROUNDS = {None: 1, 'countersign': 2, 'tiebreak': 3}


class Conflict(Exception):
    """The request is valid but the claim or configuration is no longer in the state it assumed (HTTP 409)."""


class NotFound(KeyError):
    """No such claim, or not one of yours (HTTP 404)."""


def agents_from_access(access):
    """agents() for the dispatcher, read from the user store on every call so a demotion applies at once."""
    def agents():
        out = []
        for user in access.store.list_users():
            try:
                perms = permissions.effective_permissions(user.level, user.grants, user.revokes)
                active = user.active
            except ValueError:
                perms, active = frozenset(), False                   # a corrupt record gets no work
            out.append(Agent(user.badge_id, perms, active))
        return out
    return agents


def _iso(ts):
    return datetime.fromtimestamp(ts, tz=timezone.utc).isoformat()


def _stage(doc):
    return (doc.get('signoff') or {}).get('stage')


def _round(doc):
    return ROUNDS.get(_stage(doc), 1)


def _is_high(finding):
    return finding.get('severity') != 'medium'          # an unknown severity counts as high, as in triage


def _flagged(doc):
    return [r for r in doc['results'] if r['status'] in REVIEWABLE]


def _targets(doc):
    """The rule ids still to be decided in this claim's current round: every flagged finding in round one, then only the
    high-severity ones (the medium ones were settled by one person)."""
    rows = _flagged(doc)
    if _stage(doc) is not None:
        rows = [r for r in rows if _is_high(r)]
    return [r['rule_id'] for r in rows]


def _latest_actions(doc, rnd=None):
    """The latest action per rule, from all rounds or from one."""
    latest = {}
    for d in doc.get('decisions') or []:
        if rnd is None or d.get('round', 1) == rnd:
            latest[d['rule_id']] = d['action']
    return latest


def _short(value):
    text = json.dumps(value, sort_keys=True, separators=(',', ':'))
    return text if len(text) <= LOG_VALUE_CHARS else 'sha256:' + hashlib.sha256(text.encode('utf-8')).hexdigest()


class QueueService:
    def __init__(self, store, dispatcher, access, securitylog, review_log, clock):
        self.store, self.dispatcher, self.access = store, dispatcher, access
        self.securitylog, self.review_log, self.clock = securitylog, review_log, clock
        self.intake = None                                   # set by the wiring; only the submit route needs it

    # ---- permissions and ownership
    def _fresh(self, principal):
        try:
            user = self.access.store.get_user(principal.badge)
        except StoreUnavailable:
            raise AuthError('unavailable') from None
        if user is None or not user.active:
            return frozenset()
        try:
            return permissions.effective_permissions(user.level, user.grants, user.revokes)
        except ValueError:
            return frozenset()

    def _require(self, principal, permission):
        perms = self._fresh(principal)
        if permission not in perms:
            raise Forbidden(permission)
        return perms

    def _mine(self, principal, claim_id, now):
        """The latest version of the claim, if it is leased to this person right now; otherwise NotFound or Conflict."""
        doc = self.store.get(claim_id)
        if doc is None:
            raise NotFound(claim_id)
        lease = doc.get('lease')
        if doc['state'] == 'leased' and lease and lease['badge_id'] == principal.badge:
            if lease['expires_at'] <= now:
                raise Conflict('lease_expired')
            return doc
        held_before = any(e.get('actor') == principal.badge or (e.get('detail') or {}).get('badge_id') == principal.badge
                          for e in doc['events'] if e['to'] in ('leased', 'ready', 'decided'))
        if held_before:
            raise Conflict('lease_lost')
        raise NotFound(claim_id)

    def routing_config(self):
        doc = self.store.latest_config()
        return rc.DEFAULT if doc is None else rc.from_doc(doc)

    # ---- the agent's side
    @staticmethod
    def _actions(perms, finding, latest):
        if finding['status'] not in REVIEWABLE or 'claims.decide' not in perms:
            return None
        if latest.get(finding['rule_id']) in RESOLVING:
            return None
        if finding.get('severity') != 'medium' and 'claims.decide_high' not in perms:
            return None
        return list(('confirm_issue', 'dismiss_with_reason', 'request_information', 'mark_corrected_for_recheck'))

    def _view(self, perms, doc):
        key = self.access.settings.pii_key
        shaped = masking.shape_claim(doc['claim'], doc['results'], perms, key)
        mapping = masking.identifier_map(doc['claim'], key)
        texts = (doc.get('explanation') or {}).get('findings', {})
        rnd, targets = _round(doc), set(_targets(doc))
        latest = _latest_actions(doc, rnd)
        raw = {r['rule_id']: r for r in doc['results']}
        findings = []
        for r in shaped['results']:
            row = dict(r)
            original = raw[r['rule_id']]
            if original['status'] in REVIEWABLE:
                if r['rule_id'] in texts:
                    row['ai_explanation'] = {'text': masking.scrub(texts[r['rule_id']]['text'], mapping),
                                             'source': texts[r['rule_id']]['source']}
                if r['rule_id'] in targets:
                    action = latest.get(r['rule_id'])
                    row['review_state'] = 'unreviewed' if action is None else 'resolved' if action in RESOLVING else 'awaiting_follow_up'
                    actions = self._actions(perms, original, latest)
                    if actions:
                        row['allowed_actions'] = actions
                else:
                    row['review_state'] = 'resolved'                    # settled in an earlier round
            findings.append(row)
        receipt = doc['receipt']
        view = {'claim_id': doc['claim_id'], 'version': doc['version'], 'lane': receipt['lane'], 'score': receipt['score'],
                'eligibility': receipt['eligibility'], 'escalated': bool(doc.get('escalated')), 'signoff_stage': _stage(doc),
                'leased_until': (doc.get('lease') or {}).get('expires_at'), 'claim': shaped['claim'], 'findings': findings}
        if doc.get('advisory'):
            # advisory (extension) results: shown for information, shaped by the same masking, and never offered as decidable
            view['advisory'] = masking.shape_claim(doc['claim'], doc['advisory'], perms, key)['results']
        if not triage.flagged(doc['results']) and 'claims.decide' in perms:
            view['allowed_actions'] = list(GREEN_ACTIONS)
        if masking.leaks(view, masking.identifier_map(doc['claim'], key)):
            raise masking.RedactionError('an identifier survived masking')
        return view

    def inbox(self, principal):
        perms = self._require(principal, 'claims.decide')
        return [self._view(perms, d) for d in self.store.inbox(principal.badge)]

    def next(self, principal):
        perms = self._require(principal, 'claims.decide')
        doc = self.dispatcher.next_for(principal.badge)
        return None if doc is None else self._view(perms, doc)

    def heartbeat(self, principal):
        self._require(principal, 'claims.decide')
        now = self.clock()
        return self.store.heartbeat(principal.badge, now, now + self.routing_config().lease_seconds)

    # ---- decisions
    def decide_finding(self, principal, claim_id, rule_id, action, reason):
        now = self.clock()
        perms = self._require(principal, 'claims.decide')
        doc = self._mine(principal, claim_id, now)
        sign = doc.get('signoff') or {}
        if principal.badge in (sign.get('first_by'), sign.get('second_by')):
            raise Conflict('same_person')                       # the first signer never countersigns, nor the second the tie-break
        results = doc['results']
        finding = next((r for r in results if r['rule_id'] == rule_id), None)
        if finding is None:
            raise NotFound(rule_id)
        if finding['status'] not in REVIEWABLE:
            raise ValueError('only a flagged finding can be decided')
        if rule_id not in _targets(doc):
            raise ValueError('this finding was settled in an earlier round; only high-severity findings are countersigned')
        if _is_high(finding) and 'claims.decide_high' not in perms:
            raise Forbidden('claims.decide_high')
        rnd = _round(doc)
        if _latest_actions(doc, rnd).get(rule_id) in RESOLVING:
            raise Conflict('already_decided')
        decision = {'claim_id': claim_id, 'rule_id': rule_id, 'action': action, 'actor': principal.badge, 'reason': reason,
                    'created_at': _iso(now), 'original_status': finding['status']}
        try:
            review_workflow.validate_decision(decision, review_workflow.index_findings(results))
        except review_workflow.DecisionError as e:
            raise ValueError(str(e)[:300]) from None
        updated = self.store.add_decision(claim_id, doc['version'], principal.badge, now,
                                          {'rule_id': rule_id, 'action': action, 'actor': principal.badge, 'reason': reason, 'at': now,
                                           'round': rnd})
        if updated is None:
            raise Conflict('lease_lost')
        try:
            self.review_log.append_review_decisions([decision])
        except OSError:
            raise AuthError('unavailable') from None
        self.access.record('decision', badge_id=principal.badge, claim_id=claim_id, rule_id=rule_id, action=action)
        targets, latest = _targets(updated), _latest_actions(updated, rnd)
        state = 'leased'
        if all(latest.get(rid) in RESOLVING for rid in targets):
            state = self._finish(principal, updated, rnd, targets, latest, now)
        return {'recorded': True, 'claim_state': state, 'review_state': 'resolved' if action in RESOLVING else 'awaiting_follow_up'}

    def _move(self, doc, to, principal, now, set_fields, detail):
        moved = self.store.transition(doc['claim_id'], doc['version'], 'leased', to, principal.badge, now, detail=detail,
                                      set_fields=set_fields, holder=principal.badge)
        if moved is None:
            raise Conflict('lease_lost')
        return to

    def _signed(self, doc, principal, stage, outcome):
        self.access.record('claim_signoff', claim_id=doc['claim_id'], badge_id=principal.badge, stage=stage, outcome=outcome)

    def _finish(self, principal, doc, rnd, targets, latest, now):
        """Every finding of this round is resolved: decide the claim, ask for a countersignature, or settle a disagreement."""
        badge, sign = principal.badge, doc.get('signoff') or {}
        high = {r['rule_id'] for r in _flagged(doc) if _is_high(r)}
        detail = {'findings': len(targets)}
        if rnd == 1:
            if not high:
                return self._move(doc, 'decided', principal, now, {'decided_by': badge}, detail)
            signoff = {'stage': 'countersign', 'first_by': badge, 'actions': {rid: latest[rid] for rid in sorted(high)}}
            state = self._move(doc, 'awaiting_countersign', principal, now, {'lease': None, 'signoff': signoff},
                               {**detail, 'event': 'first_signature'})
            self._signed(doc, principal, 'first', 'pending')
            return state
        if rnd == 2:
            if all(latest[rid] == sign['actions'][rid] for rid in targets):
                state = self._move(doc, 'decided', principal, now,
                                   {'decided_by': badge, 'signoff': {**sign, 'stage': 'done', 'second_by': badge, 'outcome': 'agreed'}}, detail)
                self._signed(doc, principal, 'countersign', 'agreed')
                return state
            state = self._move(doc, 'ready', principal, now,
                               {'lease': None, 'escalated': True,
                                'signoff': {**sign, 'stage': 'tiebreak', 'second_by': badge, 'outcome': 'disagreed',
                                            'second_actions': {rid: latest[rid] for rid in sorted(targets)}}},
                               {'event': 'signoff_disagreed', 'badge_id': badge, 'first_by': sign.get('first_by', '')})
            self._signed(doc, principal, 'countersign', 'disagreed')
            return state
        state = self._move(doc, 'decided', principal, now,
                           {'decided_by': badge, 'signoff': {**sign, 'stage': 'done', 'third_by': badge, 'outcome': 'tiebreak'}}, detail)
        self._signed(doc, principal, 'tiebreak', 'final')
        return state

    def decide_green(self, principal, claim_id, action):
        now = self.clock()
        self._require(principal, 'claims.decide')
        if action not in GREEN_ACTIONS:
            raise ValueError('action must be verify_clear or escalate')
        doc = self._mine(principal, claim_id, now)
        if triage.flagged(doc['results']):
            raise ValueError('this claim has findings to decide one by one')
        if action == 'verify_clear':
            moved = self.store.transition(claim_id, doc['version'], 'leased', 'decided', principal.badge, now,
                                          detail={'findings': 0}, set_fields={'decided_by': principal.badge}, holder=principal.badge)
            if moved is None:
                raise Conflict('lease_lost')
            self.access.record('claim_decided_green', badge_id=principal.badge, claim_id=claim_id, action='verify_clear')
            return {'claim_state': 'decided'}
        moved = self.store.transition(claim_id, doc['version'], 'leased', 'ready', principal.badge, now,
                                      detail={'event': 'escalated', 'badge_id': principal.badge},
                                      set_fields={'lease': None, 'escalated': True}, holder=principal.badge)
        if moved is None:
            raise Conflict('lease_lost')
        self.access.record('claim_decided_green', badge_id=principal.badge, claim_id=claim_id, action='escalate')
        return {'claim_state': 'ready'}

    # ---- the administrator's side
    def dashboard(self, principal):
        self._require(principal, 'queue.view')
        now = self.clock()
        cfg = self.routing_config()
        agents = [a for a in self.dispatcher.agents() if a.active and a.badge_id in cfg.on_shift]
        senior = [a for a in agents if 'claims.decide_high' in a.permissions]
        junior = [a for a in agents if 'claims.decide' in a.permissions]
        oldest = {'decide': 0.0, 'decide_high': 0.0}
        waiting = {'decide': 0, 'decide_high': 0}
        pool = self.store.by_state('ready', 5000) + self.store.by_state('awaiting_countersign', 5000)
        for d in pool:
            kind = 'decide_high' if d['receipt']['eligibility'] == 'decide_high' or d.get('escalated') or d.get('signoff') else 'decide'
            waiting[kind] += 1
            oldest[kind] = max(oldest[kind], now - d['state_at'])
        shortages = []
        if waiting['decide_high'] and not senior:
            shortages.append('high-severity claims are waiting and nobody on shift may decide them')
        if waiting['decide'] and not junior:
            shortages.append('claims are waiting and nobody is on shift')
        signed = [d for d in pool if (d.get('signoff') or {}).get('stage') in ('countersign', 'tiebreak')]
        if any(not [a for a in senior if a.badge_id not in {d['signoff'].get('first_by'), d['signoff'].get('second_by')}] for d in signed):
            shortages.append('claims are waiting for a countersignature and no other senior is on shift')
        inboxes = {a.badge_id: self.store.inbox_load(a.badge_id)['count'] for a in agents}
        return {'counts': self.store.counts(), 'waiting': waiting, 'awaiting_countersign': sum(1 for d in signed), 'oldest_waiting_seconds': oldest, 'on_shift': len(agents),
                'on_shift_senior': len(senior), 'inbox_sizes': inboxes, 'shortages': shortages, 'config_version': cfg.version,
                'slice_size': cfg.slice_size}

    def submit_claims(self, principal, claims):
        """Hand claims to intake (validate, run the engine, write the receipt). The outbox marker intake leaves is what the relay
        sweep publishes to the workers, so this call does no processing itself. One claim failing never stops the others."""
        self._require(principal, 'routing.manage')
        if self.intake is None:
            raise ValueError('this server was started without a claim intake')
        if type(claims) is not list or not claims or len(claims) > MAX_SUBMIT:
            raise ValueError(f'send between 1 and {MAX_SUBMIT} claims at a time')
        self.access.record('queue_admin', actor=principal.badge, command='submit_http')
        rows = []
        for claim in claims:
            claim_id = claim.get('claim_id') if isinstance(claim, dict) else None
            try:
                receipt = self.intake.submit(claim)
            except ValueError as e:
                rows.append({'claim_id': claim_id if isinstance(claim_id, str) else None, 'accepted': False, 'reason': str(e)[:200]})
                continue
            rows.append({'claim_id': claim_id, 'accepted': True, 'lane': receipt['lane'], 'score': receipt['score']})
        return rows

    def get_config(self, principal):
        self._require(principal, 'routing.manage')
        stored = self.store.latest_config()
        return {'stored_version': stored['version'] if stored else 0, 'config': rc.to_doc(self.routing_config())}

    def set_config(self, principal, changes, expected_version):
        self._require(principal, 'routing.manage')
        if type(changes) is not dict or not changes:
            raise ValueError('changes must be a non-empty object')
        unknown = set(changes) - CONFIG_FIELDS
        if unknown:
            raise ValueError('these settings cannot be changed: ' + ', '.join(sorted(str(k) for k in unknown)))
        stored = self.store.latest_config()
        stored_version = stored['version'] if stored else 0
        if expected_version != stored_version:
            raise Conflict('stale_configuration')
        current = self.routing_config()
        fixed = dict(changes)
        if 'on_shift' in fixed:
            fixed['on_shift'] = tuple(fixed['on_shift'])
        if 'exclusions' in fixed:
            fixed['exclusions'] = tuple(tuple(pair) for pair in fixed['exclusions'])
        new = rc.validate(dataclasses.replace(current, version=stored_version + 1, **fixed))
        if not self.store.put_config(rc.to_doc(new), stored_version):
            raise Conflict('stale_configuration')
        before_doc, after_doc = rc.to_doc(current), rc.to_doc(new)
        names = sorted(k for k in after_doc if k != 'version' and before_doc.get(k) != after_doc[k])
        self.access.record('routing_config_changed', actor=principal.badge, version=new.version,
                           before={k: _short(before_doc.get(k)) for k in names}, after={k: _short(after_doc[k]) for k in names})
        return new
