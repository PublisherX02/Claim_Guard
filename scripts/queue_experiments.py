"""The scale experiment for the work queue: 500 claims, 20 agents, a slice of 25 each, run on a simulated clock.

    python scripts/queue_experiments.py --out outputs/defense/queue.json

What is real: the rule engine's results on the public claims, the triage formula, the pipeline, the AI explanation step's cache and guard,
the dispatcher (its balanced dealing, eligibility and conflict rules) and the store. What is simulated, and said so in the output: the
people (each decides a claim after a service time set by how many findings it has, and dismisses a finding with a fixed probability) and
the model (a stand-in that always answers with one template). So the figures describe how the queue behaves under that workload; they say
nothing about how real reviewers would decide. The numbers that must hold whatever the workload are reported as checks: no claim sits in
two inboxes, no agent holds more than a slice, and nobody decides a claim they may not.
"""
import argparse
import dataclasses
import json
import random
import statistics
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from engine_core import config, load_jsonl  # noqa: E402
from workqueue import pipeline, routing_config as rc, shadow, triage  # noqa: E402
from workqueue.breaker import CircuitBreaker  # noqa: E402
from workqueue.dispatcher import Agent, Dispatcher  # noqa: E402
from workqueue.explain import ExplainStep, default_guard, deterministic_text  # noqa: E402
from workqueue.intake import Intake  # noqa: E402
from workqueue.store import MemoryQueueStore  # noqa: E402

L2 = frozenset({'claims.decide'})
L3 = frozenset({'claims.decide', 'claims.decide_high'})
TEMPLATE = 'The value {value} on line {line} does not satisfy this rule.'
_CACHE = {}


class NullLog:
    def record(self, *args, **kwargs):
        return None


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


def commit_hash(root=ROOT):
    """The commit of the working tree, read from .git directly (best effort)."""
    try:
        git = root / '.git'
        gitdir = Path(git.read_text(encoding='utf-8').split(':', 1)[1].strip()) if git.is_file() else git
        if not gitdir.is_absolute():
            gitdir = (root / gitdir).resolve()
        head = (gitdir / 'HEAD').read_text(encoding='utf-8').strip()
        if not head.startswith('ref:'):
            return head
        ref = head.split(':', 1)[1].strip()
        bases = [gitdir]
        if (gitdir / 'commondir').exists():
            bases.append((gitdir / (gitdir / 'commondir').read_text(encoding='utf-8').strip()).resolve())
        for base in bases:
            if (base / ref).exists():
                return (base / ref).read_text(encoding='utf-8').strip()
            packed = base / 'packed-refs'
            if packed.exists():
                for line in packed.read_text(encoding='utf-8').split('\n'):
                    if line.endswith(' ' + ref):
                        return line.split(' ', 1)[0]
    except (OSError, IndexError):
        pass
    return 'unknown'


def public_claims():
    """Every public claim with the engine's results, under split-prefixed ids (built once per process)."""
    if 'claims' not in _CACHE:
        from yara_engine import evaluate
        cfg = config(str(ROOT))
        rows = {}
        for split in ('development', 'validation', 'stress'):
            for claim in load_jsonl(str(ROOT / 'data' / split / 'claims.jsonl')):
                claim = dict(claim, claim_id=f'{split[:3]}-{claim["claim_id"]}')
                rows[claim['claim_id']] = (claim, evaluate(claim, cfg, []))
        _CACHE['claims'] = rows
    return _CACHE['claims']


class StandInModel:
    def __init__(self):
        self.calls = 0

    def __call__(self, request, deadline):
        self.calls += 1
        return TEMPLATE


def _share(busy, group, total):
    """Mean share of the run that agents in this group spent working."""
    if not group or not total:
        return None
    return round(statistics.fmean(busy[b] / total for b in group), 4)


def _spread(values):
    values = list(values)
    if not values:
        return {'agents': 0, 'min': None, 'max': None, 'cv': None}
    mean = statistics.fmean(values)
    return {'agents': len(values), 'min': min(values), 'max': max(values), 'cv': round(statistics.pstdev(values) / mean, 4) if mean else 0.0}


def run(n_claims=500, n_agents=20, slice_size=25, seed=1, l3=4, extra_granted=0, service_base=60.0, service_per_finding=45.0,
        dismiss_prob=0.1, signoff=False, disagree_prob=0.1):
    rng = random.Random(seed)
    pool = public_claims()
    ids = sorted(pool)
    if n_claims > len(ids):
        raise ValueError(f'only {len(ids)} public claims are available')
    chosen = rng.sample(ids, n_claims)
    results_of = {cid: pool[cid][1] for cid in chosen}

    store, clock = MemoryQueueStore(), Clock()
    badges = [f'A{i:02d}' for i in range(1, n_agents + 1)]
    agents = [Agent(b, L3 if i < l3 + 0 else (L3 if i < l3 + extra_granted else L2)) for i, b in enumerate(badges)]
    cfg = rc.validate(dataclasses.replace(rc.DEFAULT, version=1, on_shift=tuple(badges), slice_size=slice_size,
                                          low_water=max(1, slice_size // 5), lease_seconds=86400, ai_per_minute=600, ai_daily_budget=100000))
    store.put_config(rc.to_doc(cfg), 0)

    model = StandInModel()
    step = ExplainStep(store, model, default_guard, CircuitBreaker(clock), clock, random.Random(seed), lambda: cfg, deterministic_text,
                       model_name='stand-in')
    steps = type('Steps', (), {'explain': step})()
    intake = Intake(store, lambda claim: results_of[claim['claim_id']], NullLog(), clock, 'experiment-pack', 'experiment')
    dispatcher = Dispatcher(store, lambda: agents, clock, rng_seed=seed)
    by_badge = {a.badge_id: a for a in agents}

    lanes, eligibilities, flagged_findings = {}, {}, 0
    for cid in chosen:
        receipt = intake.submit(pool[cid][0])
        lanes[receipt['lane']] = lanes.get(receipt['lane'], 0) + 1
        eligibilities[receipt['eligibility']] = eligibilities.get(receipt['eligibility'], 0) + 1
        flagged_findings += len(triage.flagged(results_of[cid]))
        pipeline.advance(store, cid, 1, steps, clock())

    inbox = {b: [] for b in badges}          # claim ids each agent holds, in the order they were dealt; kept here, not re-read
    held = set()                              # every claim id currently in some inbox
    working = {}                              # badge -> (claim_id, finish_time)
    seen_by = {b: set() for b in badges}
    points = {b: 0 for b in badges}
    busy = {b: 0.0 for b in badges}
    handled = {b: 0 for b in badges}
    stats = {'max_inbox': 0, 'overlaps': 0, 'decisions': 0, 'violations': 0, 'max_ready': {'decide': 0, 'decide_high': 0},
             'signatures': 0, 'same_person': 0, 'escalations': 0}
    signers = {}
    step_no = 0
    while True:
        counts = store.counts()
        ready_total = sum(n for k, n in counts.items() if k.startswith('ready|') or k.startswith('awaiting_countersign|'))
        for kind in ('decide', 'decide_high'):
            stats['max_ready'][kind] = max(stats['max_ready'][kind], sum(
                n for k, n in counts.items() if (k.startswith('ready|') or k.startswith('awaiting_countersign|')) and k.endswith('|' + kind)))
        if ready_total and any(len(inbox[b]) < cfg.low_water for b in badges):
            dispatcher.rng_seed = seed * 1_000_003 + step_no
            step_no += 1
            for cid, badge, _ in dispatcher.deal(now=clock(), full=False).assigned:
                if cid in held:
                    stats['overlaps'] += 1
                held.add(cid)
                inbox[badge].append(cid)
            stats['max_inbox'] = max(stats['max_inbox'], max(len(v) for v in inbox.values()))
        for badge in badges:
            if badge in working:
                continue
            doing = {c for c, _ in working.values()}
            waiting = [c for c in inbox[badge] if c not in doing]
            if waiting:
                doc = store.get(waiting[0])
                stage = (doc.get('signoff') or {}).get('stage')
                work = triage.flagged(doc['results'])
                if stage is not None:
                    work = [r for r in work if r['severity'] != 'medium']          # a countersigner re-decides only the high findings
                took = service_base + service_per_finding * len(work)
                working[badge] = (doc['claim_id'], clock() + took)
                busy[badge] += took
                seen_by[badge].add(doc['claim_id'])
        if not working:
            break
        clock.t = min(finish for _, finish in working.values())
        for badge in [b for b, (_, finish) in working.items() if finish <= clock.t]:
            cid, _ = working.pop(badge)
            doc = store.get(cid)
            if doc['receipt']['eligibility'] == 'decide_high' and 'claims.decide_high' not in by_badge[badge].permissions:
                stats['violations'] += 1
            sign = doc.get('signoff') or {}
            stage = sign.get('stage')
            rnd = {None: 1, 'countersign': 2, 'tiebreak': 3}[stage]
            flagged = triage.flagged(doc['results'])
            high = [r for r in flagged if r['severity'] != 'medium']
            rows = flagged if stage is None else high
            disagreed, taken = False, {}
            for r in rows:
                if stage == 'countersign':
                    action = sign['actions'][r['rule_id']]
                    if rng.random() < disagree_prob:
                        action = 'dismiss_with_reason' if action == 'confirm_issue' else 'confirm_issue'
                        disagreed = True
                else:
                    action = 'dismiss_with_reason' if rng.random() < dismiss_prob else 'confirm_issue'
                taken[r['rule_id']] = action
                store.add_decision(cid, 1, badge, clock.t - 1, {'rule_id': r['rule_id'], 'action': action, 'actor': badge, 'reason': 'sim',
                                                                  'at': clock.t, 'round': rnd})
            stats['signatures'] += 1
            if badge in signers.setdefault(cid, set()):
                stats['same_person'] += 1
            signers[cid].add(badge)
            inbox[badge].remove(cid)
            held.discard(cid)
            handled[badge] += 1
            points[badge] += doc['receipt']['score']
            if signoff and stage is None and high:
                first = {r['rule_id']: taken[r['rule_id']] for r in high}
                store.transition(cid, 1, 'leased', 'awaiting_countersign', badge, clock.t, holder=badge,
                                 set_fields={'lease': None, 'signoff': {'stage': 'countersign', 'first_by': badge, 'actions': first}})
                continue
            if signoff and stage == 'countersign' and disagreed:
                stats['escalations'] += 1
                store.transition(cid, 1, 'leased', 'ready', badge, clock.t, holder=badge,
                                 set_fields={'lease': None, 'escalated': True, 'signoff': {**sign, 'stage': 'tiebreak', 'second_by': badge}})
                continue
            store.transition(cid, 1, 'leased', 'decided', badge, clock.t, set_fields={'decided_by': badge}, holder=badge)
            stats['decisions'] += 1

    left = store.by_state('ready', 100000) + store.by_state('awaiting_countersign', 100000)
    sources = [f['source'] for d in store.by_state('decided', 100000) for f in ((d.get('explanation') or {}).get('findings') or {}).values()]
    seniors = [b for b in badges if 'claims.decide_high' in by_badge[b].permissions]
    juniors = [b for b in badges if b not in seniors]
    hits, fresh = sources.count('cache'), sources.count('ai')
    return {
        'seed': seed, 'n_claims': n_claims, 'n_agents': n_agents, 'slice_size': slice_size, 'senior_agents': len(seniors),
        'of_which_granted_l2': extra_granted,
        'lane_counts': dict(sorted(lanes.items())), 'eligibility_counts': dict(sorted(eligibilities.items())),
        'checks': {
            'slice_never_exceeded': stats['max_inbox'] <= slice_size, 'max_inbox_seen': stats['max_inbox'],
            'claims_in_two_inboxes': stats['overlaps'], 'decisions': stats['decisions'],
            'decisions_by_agents_not_allowed': stats['violations'],
            'signatures': stats['signatures'], 'claims_signed_twice_by_one_person': stats['same_person'],
            'escalations_to_a_third_senior': stats['escalations'],
            'eligibility_respected_pct': 100.0 if not stats['decisions'] else round(100 * (1 - stats['violations'] / stats['decisions']), 2),
        },
        'distinct_claims_seen_per_agent': _spread(len(seen_by[b]) for b in badges),
        'fairness_points': {'all_agents': _spread(points[b] for b in badges), 'senior': _spread(points[b] for b in seniors),
                            'junior': _spread(points[b] for b in juniors)},
        'drain': {'simulated_seconds': round(clock.t, 1), 'simulated_hours': round(clock.t / 3600, 2), 'claims_left_undecided': len(left),
                  'left_requiring_senior': sum(1 for d in left if d['receipt']['eligibility'] == 'decide_high')},
        'max_waiting_in_pool': stats['max_ready'],
        'utilization': {'senior': _share(busy, seniors, clock.t), 'junior': _share(busy, juniors, clock.t)},
        'cache': {'flagged_findings': flagged_findings, 'model_calls': model.calls, 'cache_hits': hits, 'fresh_explanations': fresh,
                  'hit_rate': round(hits / (hits + fresh), 4) if hits + fresh else None},
        'shadow_agreement': {k: (round(v, 4) if isinstance(v, float) else v) for k, v in shadow.agreement(store).items()},
        'assumptions': {'two_person_signoff': signoff, 'countersigner_disagrees_with_probability': disagree_prob if signoff else None,
                        'people_are_simulated': True, 'service_seconds': f'{service_base:g} + {service_per_finding:g} per flagged finding',
                        'finding_dismissed_with_probability': dismiss_prob, 'model_is_a_stand_in': True,
                        'leases_do_not_expire': True, 'all_agents_on_shift_for_the_whole_run': True},
    }


def experiments(seed=1, n_claims=500, n_agents=20, slice_size=25):
    base = run(n_claims, n_agents, slice_size, seed, l3=4)
    seeds = [run(n_claims, n_agents, slice_size, s, l3=4)['checks'] for s in range(1, 6)]
    return {
        'commit': commit_hash(),
        'workload': {'claims': n_claims, 'agents': n_agents, 'slice_size': slice_size, 'seed': seed},
        'base_4_senior_of_20': base,
        'with_4_more_l2_granted_the_senior_flag': run(n_claims, n_agents, slice_size, seed, l3=4, extra_granted=4),
        'no_senior_on_shift': run(n_claims, n_agents, slice_size, seed, l3=0),
        'with_two_person_signoff': run(n_claims, n_agents, slice_size, seed, l3=4, signoff=True),
        'with_two_person_signoff_and_4_more_l2_granted': run(n_claims, n_agents, slice_size, seed, l3=4, extra_granted=4, signoff=True),
        'invariants_over_five_seeds': {'overlaps': [c['claims_in_two_inboxes'] for c in seeds],
                                       'eligibility_respected_pct': [c['eligibility_respected_pct'] for c in seeds],
                                       'slice_never_exceeded': [c['slice_never_exceeded'] for c in seeds]},
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--out', default='outputs/defense/queue.json')
    parser.add_argument('--seed', type=int, default=1)
    parser.add_argument('--claims', type=int, default=500)
    parser.add_argument('--agents', type=int, default=20)
    parser.add_argument('--slice', type=int, default=25)
    args = parser.parse_args(argv)
    result = experiments(args.seed, args.claims, args.agents, args.slice)
    path = Path(args.out)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    base = result['base_4_senior_of_20']
    print(f"wrote {path}: base drain {base['drain']['simulated_hours']} h, overlaps {base['checks']['claims_in_two_inboxes']}, "
          f"eligibility {base['checks']['eligibility_respected_pct']} %")
    return 0


if __name__ == '__main__':
    sys.exit(main())
