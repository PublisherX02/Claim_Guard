"""An independent checker for a dealing plan. It does not re-derive the order the dispatcher chose: it asserts the invariants any
correct plan must satisfy, so a bug that is consistent inside the dispatcher cannot hide.

check_plan(ready, agents, loads, cfg, plan, max_per_agent=None) raises AssertionError describing the first broken invariant.
Documents look like those in the store: {claim_id, version, receipt: {score, eligibility}, claim: {patient_id}, prior_deciders}.
"""


def needed(doc):
    return 'claims.decide_high' if doc['receipt']['eligibility'] == 'decide_high' else 'claims.decide'


def allowed(agent, doc, cfg):
    """May this agent be given this claim, ignoring capacity?"""
    pair = (agent.badge_id, doc['claim']['patient_id'])
    return (agent.active and agent.badge_id in cfg.on_shift and needed(doc) in agent.permissions
            and pair not in set(map(tuple, cfg.exclusions)) and agent.badge_id not in doc.get('prior_deciders', ()))


def check_plan(ready, agents, loads, cfg, plan, max_per_agent=None):
    by_key = {(d['claim_id'], d['version']): d for d in ready}
    by_badge = {a.badge_id: a for a in agents}
    given = {}
    seen = set()
    for claim_id, badge, version in plan:
        assert (claim_id, version) in by_key, f'{claim_id} v{version} is not in the pool'
        assert claim_id not in seen, f'{claim_id} is dealt twice'
        seen.add(claim_id)
        assert badge in by_badge, f'{badge} is not an agent'
        agent, doc = by_badge[badge], by_key[(claim_id, version)]
        assert agent.active, f'{badge} is not active'
        assert badge in cfg.on_shift, f'{badge} is not on shift'
        assert needed(doc) in agent.permissions, f'{badge} lacks {needed(doc)} for {claim_id}'
        assert (badge, doc['claim']['patient_id']) not in set(map(tuple, cfg.exclusions)), f'{badge} is excluded from {claim_id}'
        assert badge not in doc.get('prior_deciders', ()), f'{badge} already decided an earlier version of {claim_id}'
        given[badge] = given.get(badge, 0) + 1
    for badge, n in given.items():
        room = cfg.slice_size - loads.get(badge, {'count': 0})['count']
        assert n <= room, f'{badge} gets {n} but has room for {room}'
        if max_per_agent is not None:
            assert n <= max_per_agent, f'{badge} gets {n}, more than {max_per_agent}'
    # nothing is left in the pool that someone could still have taken
    cap = max_per_agent if max_per_agent is not None else cfg.slice_size
    for d in ready:
        if d['claim_id'] in seen:
            continue
        for a in agents:
            room = min(cfg.slice_size - loads.get(a.badge_id, {'count': 0})['count'], cap) - given.get(a.badge_id, 0)
            assert not (allowed(a, d, cfg) and room > 0), f'{d["claim_id"]} was left although {a.badge_id} could take it'


def spread_of_points(ready, agents, loads, cfg, plan):
    """(spread, largest dealt score) of the agents' point totals after the plan, for the fairness check."""
    score = {(d['claim_id'], d['version']): d['receipt']['score'] for d in ready}
    totals = {a.badge_id: loads.get(a.badge_id, {'points': 0})['points'] for a in agents}
    biggest = 0
    for claim_id, badge, version in plan:
        totals[badge] += score[(claim_id, version)]
        biggest = max(biggest, score[(claim_id, version)])
    values = list(totals.values())
    return (max(values) - min(values) if values else 0), biggest
