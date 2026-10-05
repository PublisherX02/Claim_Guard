"""The dispatcher: deals ready claims into the personal inboxes of eligible agents, fairly, reproducibly and without waiting.

An agent never sees the whole pool. Each agent holds at most `slice_size` claims, leased to them one by one; the dispatcher tops an
inbox up when it drops below `low_water`, so nobody waits for work while work exists. Nobody assigns a particular claim to a
particular person: an administrator sets capacity and shifts, the formula and a logged random seed do the rest.

plan_deal is pure: the same pool, agents, loads, configuration and seed always give the same plan. The deal document stores exactly
those inputs, so replay() can re-derive the plan later and verify() can prove the stored assignments are what the algorithm yields.
"""
import hashlib
import json
import random
import secrets
import uuid
from dataclasses import dataclass, field

from . import leases
from . import routing_config as rc

MAX_POOL = 5000
DECIDE, DECIDE_HIGH = 'claims.decide', 'claims.decide_high'
AVOID_EVENTS = ('lease_expired', 'escalated')       # whoever held or escalated a claim is the last choice to get it again


@dataclass(frozen=True)
class Agent:
    badge_id: str
    permissions: frozenset
    active: bool = True


@dataclass
class Deal:
    deal_id: str
    seed: int
    config_version: int
    planned: list = field(default_factory=list)
    assigned: list = field(default_factory=list)      # the part of the plan that was won (another dispatcher may win a claim first)


def _needed(doc):
    return DECIDE_HIGH if doc['receipt']['eligibility'] == 'decide_high' else DECIDE


def _priority(doc, cfg, now):
    waited = max(0.0, (now - doc['state_at']) / 3600)
    return doc['receipt']['score'] + cfg.aging_per_hour * waited


def _blocked(agent, doc, excluded):
    return (agent.badge_id, doc['claim']['patient_id']) in excluded or agent.badge_id in doc.get('prior_deciders', ())


def plan_deal(ready, agents, inbox_loads, cfg, seed, now, max_per_agent=None):
    """Return [(claim_id, badge_id, version)]. Pure and deterministic.

    Claims are shuffled by the seed (so ties are random but reproducible), then stably sorted by priority = score plus a little
    for every hour waited. Each claim goes to the eligible agent with the least work (sum of scores) so far; ties by the seeded
    order of agents. A claim that was handed back from an agent is offered to someone else first when anyone else is eligible.
    """
    badges = [a.badge_id for a in agents]
    if len(set(badges)) != len(badges):
        raise ValueError('an agent appears twice')
    excluded = {tuple(pair) for pair in cfg.exclusions}
    considered = [a for a in agents if a.active and a.badge_id in cfg.on_shift]
    zero = {'count': 0, 'points': 0}
    room = {a.badge_id: cfg.slice_size - inbox_loads.get(a.badge_id, zero)['count'] for a in considered}
    if max_per_agent is not None:
        room = {b: min(n, max_per_agent) for b, n in room.items()}
    load = {a.badge_id: inbox_loads.get(a.badge_id, zero)['points'] for a in considered}
    rng = random.Random(seed)
    order = list(ready)
    rng.shuffle(order)
    order.sort(key=lambda d: -_priority(d, cfg, now))
    pool = sorted(considered, key=lambda a: a.badge_id)
    rng.shuffle(pool)
    rank = {a.badge_id: i for i, a in enumerate(pool)}
    out = []
    for d in order:
        ok = [a for a in pool if _needed(d) in a.permissions and room[a.badge_id] > 0 and not _blocked(a, d, excluded)]
        fresh = [a for a in ok if a.badge_id not in d.get('avoid', ())]
        ok = fresh or ok
        if not ok:
            continue
        pick = min(ok, key=lambda a: (load[a.badge_id], rank[a.badge_id]))
        out.append((d['claim_id'], pick.badge_id, d['version']))
        room[pick.badge_id] -= 1
        load[pick.badge_id] += d['receipt']['score']
    return out


def _ids_hash(ready):
    pairs = sorted([d['claim_id'], d['version']] for d in ready)
    return hashlib.sha256(json.dumps(pairs, separators=(',', ':')).encode('utf-8')).hexdigest()


def _snapshot(doc, prior, avoid):
    """The part of a ready claim the plan depends on, small enough to keep in the deal document."""
    return {'claim_id': doc['claim_id'], 'version': doc['version'], 'state_at': doc['state_at'],
            'score': doc['receipt']['score'], 'eligibility': doc['receipt']['eligibility'],
            'patient_id': doc['claim'].get('patient_id', ''), 'prior_deciders': sorted(prior), 'avoid': sorted(avoid)}


def _doc_of(snap):
    return {'claim_id': snap['claim_id'], 'version': snap['version'], 'state_at': snap['state_at'],
            'receipt': {'score': snap['score'], 'eligibility': snap['eligibility']}, 'claim': {'patient_id': snap['patient_id']},
            'prior_deciders': tuple(snap['prior_deciders']), 'avoid': tuple(snap['avoid'])}


class Dispatcher:
    def __init__(self, store, agents, clock, rng_seed=None, securitylog=None):
        """agents() -> [Agent], read fresh on every deal so a demotion or deactivation takes effect at once."""
        self.store, self.agents, self.clock, self.rng_seed, self.log = store, agents, clock, rng_seed, securitylog

    # ---- configuration and inputs
    def routing_config(self):
        doc = self.store.latest_config()
        return rc.DEFAULT if doc is None else rc.from_doc(doc)

    def _config_version(self, version):
        for doc in self.store.config_history():
            if doc['version'] == version:
                return rc.from_doc(doc)
        if version == rc.DEFAULT.version and self.store.latest_config() is None:
            return rc.DEFAULT
        raise ValueError(f'routing configuration version {version} is not stored')

    def _history_of(self, doc):
        """Badges that decided an earlier version of this claim, and badges whose lease on this version already ran out."""
        sign = doc.get('signoff') or {}
        prior = {b for b in (sign.get('first_by'), sign.get('second_by')) if b}      # signers never sign the same claim twice
        for v in range(1, doc['version']):
            earlier = self.store.get(doc['claim_id'], v)
            if earlier and earlier.get('decided_by'):
                prior.add(earlier['decided_by'])
        avoid = {e['detail'].get('badge_id') for e in doc['events'] if (e.get('detail') or {}).get('event') in AVOID_EVENTS}
        return prior, {b for b in avoid if b}

    # ---- dealing
    def _deal(self, now, only=None, full=True, max_per_agent=None):
        now = self.clock() if now is None else now
        self._reclaim_ineligible(now)
        cfg = self.routing_config()
        agents = [a for a in self.agents() if only is None or a.badge_id == only]
        considered = [a for a in agents if a.active and a.badge_id in cfg.on_shift]
        loads = {a.badge_id: self.store.inbox_load(a.badge_id) for a in considered}
        considered = [a for a in considered if loads[a.badge_id]['count'] < cfg.slice_size
                      and (full or loads[a.badge_id]['count'] < cfg.low_water)]
        pool = self.store.by_state('ready', MAX_POOL) + self.store.by_state('awaiting_countersign', MAX_POOL)
        newest = self.store.latest_versions([d['claim_id'] for d in pool])
        ready = [d for d in pool if newest.get(d['claim_id'], d['version']) <= d['version']]      # only the newest version of a claim is dealt
        snaps = []
        for d in ready:
            prior, avoid = self._history_of(d)
            snap = _snapshot(d, prior, avoid)
            if d.get('escalated') or d.get('signoff'):
                snap['eligibility'] = 'decide_high'              # an escalated claim, or one awaiting a countersignature, needs a senior
            snaps.append(snap)
        seed = self.rng_seed if self.rng_seed is not None else secrets.randbits(63)
        plan = plan_deal([_doc_of(s) for s in snaps], considered, loads, cfg, seed, now, max_per_agent) if considered else []
        deal = Deal(uuid.uuid4().hex, seed, cfg.version, plan)
        if not plan:
            return deal
        expires = now + cfg.lease_seconds
        for claim_id, badge, version in plan:
            if self.store.lease(claim_id, version, badge, now, expires) is not None:
                deal.assigned.append((claim_id, badge, version))
                if self.log is not None:
                    self.log.record('claim_dealt', deal_id=deal.deal_id, claim_id=claim_id, badge_id=badge)
        self.store.append_deal({
            'deal_id': deal.deal_id, 'seed': seed, 'at': now, 'config_version': cfg.version, 'ids_hash': _ids_hash(ready),
            'max_per_agent': max_per_agent, 'full': bool(full),
            'agents': [{'badge_id': a.badge_id, 'permissions': sorted(a.permissions), 'count': loads[a.badge_id]['count'],
                        'points': loads[a.badge_id]['points']} for a in considered],
            'claims': snaps, 'planned': [list(t) for t in plan], 'assigned': [list(t) for t in deal.assigned]})
        return deal

    def deal(self, now=None, full=True):
        """Fill inboxes from the pool. full=False tops up only agents below the low-water mark (what the scheduler runs)."""
        return self._deal(now, full=full)

    def top_up_if_low(self, badge_id):
        """Fill one agent's inbox to the slice size if it holds fewer than low_water claims. Returns how many were added."""
        cfg = self.routing_config()
        if len(self.store.inbox(badge_id)) >= cfg.low_water:
            return 0
        return len(self._deal(None, only=badge_id, full=True).assigned)

    def next_for(self, badge_id):
        """One more claim for an agent who asks for it now (capacity permitting), or None."""
        deal = self._deal(None, only=badge_id, full=True, max_per_agent=1)
        if not deal.assigned:
            return None
        claim_id, _, version = deal.assigned[0]
        return self.store.get(claim_id, version)

    def expire(self, now=None):
        """Return every lease that ran out to the pool. Returns how many were returned."""
        now = self.clock() if now is None else now
        count = 0
        for doc in self.store.expired(now):
            holder = (doc.get('lease') or {}).get('badge_id', '')
            if leases.release(self.store, doc, now, 'system:dispatcher') is not None:
                count += 1
                if self.log is not None:
                    self.log.record('lease_expired', claim_id=doc['claim_id'], badge_id=holder)
        return count + self._reclaim_ineligible(now)

    def _reclaim_ineligible(self, now):
        """Take back claims held by an agent who may no longer decide them (demoted, deactivated, a permission revoked).
        A lease confers no rights of its own: the permission is checked again here and again when a decision arrives."""
        by_badge = {a.badge_id: a for a in self.agents()}
        rows = self.store.leased_summary()
        newest = self.store.latest_versions([r['claim_id'] for r in rows])
        count = 0
        for row in rows:
            agent = by_badge.get(row['badge_id'])
            need = DECIDE_HIGH if row['eligibility'] == 'decide_high' or row['escalated'] or row['signoff'] else DECIDE
            reason = 'superseded' if newest.get(row['claim_id'], row['version']) > row['version'] else None
            if reason is None and agent is not None and agent.active and need in agent.permissions:
                continue
            reason = reason or 'agent_ineligible'
            held = {'claim_id': row['claim_id'], 'version': row['version'], 'lease': {'badge_id': row['badge_id']}}
            if leases.release(self.store, held, now, 'system:dispatcher', reason=reason) is not None:
                count += 1
                if self.log is not None:
                    self.log.record('lease_reclaimed', claim_id=row['claim_id'], badge_id=row['badge_id'], reason=reason)
        return count

    # ---- reproducibility
    def replay(self, deal_id):
        """Re-derive the plan from the inputs stored in the deal and return it as [(claim_id, badge_id, version)]."""
        deal = self.store.get_deal(deal_id)
        if deal is None:
            raise ValueError('no such deal')
        cfg = self._config_version(deal['config_version'])
        agents = [Agent(a['badge_id'], frozenset(a['permissions']), True) for a in deal['agents']]
        loads = {a['badge_id']: {'count': a['count'], 'points': a['points']} for a in deal['agents']}
        ready = [_doc_of(s) for s in deal['claims']]
        return plan_deal(ready, agents, loads, cfg, deal['seed'], deal['at'], deal.get('max_per_agent'))

    def verify(self, deal_id):
        """True when the stored plan is exactly what the algorithm gives for the stored inputs and seed."""
        deal = self.store.get_deal(deal_id)
        if deal is None:
            raise ValueError('no such deal')
        return [list(t) for t in self.replay(deal_id)] == deal['planned']
