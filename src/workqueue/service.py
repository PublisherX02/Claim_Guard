"""The queue service: what an agent or an administrator may do with the queue. No HTTP in here.

Rules enforced in this file, not in the web layer:
  * a person decides only claims leased to them, and only while the lease runs; permissions are read from the user store again at
    the moment of a decision (what the session token said when the request began is not trusted);
  * a decision is written to the claim by one conditional database update that checks the lease holder and its expiry, so a decision
    that loses a race with an expiry or a re-deal is refused and writes nothing;
  * nobody assigns a particular claim to a particular person: the administrator changes capacity, shifts and the formula's numbers
    through the versioned configuration, and the dispatcher deals;
  * level 4 holds queue.view and routing.manage but not claims.decide, so whoever runs the queue cannot decide its claims.
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


def _latest_actions(doc):
    latest = {}
    for d in doc.get('decisions') or []:
        latest[d['rule_id']] = d['action']
    return latest


def _short(value):
    text = json.dumps(value, sort_keys=True, separators=(',', ':'))
    return text if len(text) <= LOG_VALUE_CHARS else 'sha256:' + hashlib.sha256(text.encode('utf-8')).hexdigest()


class QueueService:
    def __init__(self, store, dispatcher, access, securitylog, review_log, clock):
        self.store, self.dispatcher, self.access = store, dispatcher, access
        self.securitylog, self.review_log, self.clock = securitylog, review_log, clock

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
        latest = _latest_actions(doc)
        raw = {r['rule_id']: r for r in doc['results']}
        findings = []
        for r in shaped['results']:
            row = dict(r)
            original = raw[r['rule_id']]
            if original['status'] in REVIEWABLE:
                if r['rule_id'] in texts:
                    row['ai_explanation'] = {'text': masking.scrub(texts[r['rule_id']]['text'], mapping),
                                             'source': texts[r['rule_id']]['source']}
                action = latest.get(r['rule_id'])
                row['review_state'] = 'unreviewed' if action is None else 'resolved' if action in RESOLVING else 'awaiting_follow_up'
                actions = self._actions(perms, original, latest)
                if actions:
                    row['allowed_actions'] = actions
            findings.append(row)
        receipt = doc['receipt']
        view = {'claim_id': doc['claim_id'], 'version': doc['version'], 'lane': receipt['lane'], 'score': receipt['score'],
                'eligibility': receipt['eligibility'], 'escalated': bool(doc.get('escalated')),
                'leased_until': (doc.get('lease') or {}).get('expires_at'), 'claim': shaped['claim'], 'findings': findings}
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
        results = doc['results']
        finding = next((r for r in results if r['rule_id'] == rule_id), None)
        if finding is None:
            raise NotFound(rule_id)
        if finding['status'] not in REVIEWABLE:
            raise ValueError('only a flagged finding can be decided')
        if finding.get('severity') != 'medium' and 'claims.decide_high' not in perms:
            raise Forbidden('claims.decide_high')
        if _latest_actions(doc).get(rule_id) in RESOLVING:
            raise Conflict('already_decided')
        decision = {'claim_id': claim_id, 'rule_id': rule_id, 'action': action, 'actor': principal.badge, 'reason': reason,
                    'created_at': _iso(now), 'original_status': finding['status']}
        try:
            review_workflow.validate_decision(decision, review_workflow.index_findings(results))
        except review_workflow.DecisionError as e:
            raise ValueError(str(e)[:300]) from None
        updated = self.store.add_decision(claim_id, doc['version'], principal.badge, now,
                                          {'rule_id': rule_id, 'action': action, 'actor': principal.badge, 'reason': reason, 'at': now})
        if updated is None:
            raise Conflict('lease_lost')
        try:
            self.review_log.append_review_decisions([decision])
        except OSError:
            raise AuthError('unavailable') from None
        self.access.record('decision', badge_id=principal.badge, claim_id=claim_id, rule_id=rule_id, action=action)
        latest = _latest_actions(updated)
        flagged = [r['rule_id'] for r in results if r['status'] in REVIEWABLE]
        finished = all(latest.get(rid) in RESOLVING for rid in flagged)
        if finished:
            moved = self.store.transition(claim_id, doc['version'], 'leased', 'decided', principal.badge, now,
                                          detail={'findings': len(flagged)}, set_fields={'decided_by': principal.badge}, holder=principal.badge)
            if moved is None:
                raise Conflict('lease_lost')
        return {'recorded': True, 'claim_state': 'decided' if finished else 'leased',
                'review_state': 'resolved' if action in RESOLVING else 'awaiting_follow_up'}

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
        for d in self.store.by_state('ready', 5000):
            kind = 'decide_high' if d['receipt']['eligibility'] == 'decide_high' or d.get('escalated') else 'decide'
            waiting[kind] += 1
            oldest[kind] = max(oldest[kind], now - d['state_at'])
        shortages = []
        if waiting['decide_high'] and not senior:
            shortages.append('high-severity claims are waiting and nobody on shift may decide them')
        if waiting['decide'] and not junior:
            shortages.append('claims are waiting and nobody is on shift')
        inboxes = {a.badge_id: len(self.store.inbox(a.badge_id)) for a in agents}
        return {'counts': self.store.counts(), 'waiting': waiting, 'oldest_waiting_seconds': oldest, 'on_shift': len(agents),
                'on_shift_senior': len(senior), 'inbox_sizes': inboxes, 'shortages': shortages, 'config_version': cfg.version,
                'slice_size': cfg.slice_size}

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
