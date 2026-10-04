"""Measurements for the advisory extension rules, written to outputs/evaluation/extensions.json.

1. How often each rule fires on the public claims (all three splits, each claim compared with every other public claim).
2. Agreement between the engine and the independent oracle on those same claims.
3. Time per claim with no history and with a large history.
4. Scenario results: one constructed case per status a rule can give, with the expected status fixed by construction, and a
   one-sided Clopper-Pearson upper bound on the disagreement rate (a few dozen cases cannot prove a low rate; the bound says so).

    python scripts/extension_experiments.py            # full
    python scripts/extension_experiments.py --scale tiny --out /tmp/x.json
"""
import argparse
import json
import platform
import statistics
import sys
import time
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'tests'))
import extension_rules as ex
import oracle_extensions as oracle
from claim_history import InMemoryHistory
from engine_core import load_jsonl
from eval_metrics import clopper_pearson_upper
from eval_sets import read_git_head
from ext_world import claim, line

SPLITS = ('development', 'validation', 'stress')
RULES = ('E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103')


def public_claims():
    out = []
    for split in SPLITS:
        out += load_jsonl(ROOT / 'data' / split / 'claims.jsonl')
    return out


def fire_counts(claims):
    history = InMemoryHistory(claims)
    counts = {rid: Counter() for rid in RULES}
    agreement = Counter()
    for c in claims:
        got = {r['rule_id']: r for r in ex.evaluate_extensions(c, history)}
        want = oracle.oracle(c, claims) if all(isinstance(c.get(k), (str, type(None))) for k in ('patient_id', 'provider_id')) else None
        for rid in RULES:
            counts[rid][got[rid]['status']] += 1
            if want is not None:
                agreement['compared'] += 1
                agreement['agree'] += int(got[rid]['status'] == want[rid][0] and set(got[rid]['affected_line_ids']) == want[rid][1])
    return {rid: dict(sorted(v.items())) for rid, v in counts.items()}, dict(agreement)


def timing(base, history_size, repeats):
    target = claim([line(1, 'SVC-LAB'), line(2, 'SVC-THERAPY', auth='A1')], claim_id='T-1', date='2027-01-01',
                   authorizations=[{'authorization_id': 'A1', 'max_quantity': 99}])
    earlier = [claim([line(1, 'SVC-LAB')], claim_id=f'H{i}', patient=f'PAT-{i % 200}', date='2026-06-01') for i in range(history_size)]
    earlier += [claim([line(1, 'SVC-LAB')], claim_id=f'M{i}', date='2026-06-01') for i in range(50)]     # 50 earlier claims for the target's patient
    history = InMemoryHistory(earlier)
    ex.evaluate_extensions(target, history)
    samples = []
    for _ in range(repeats):
        t0 = time.perf_counter()
        ex.evaluate_extensions(target, history)
        samples.append((time.perf_counter() - t0) * 1000)
    samples.sort()
    return {'history_claims': history_size, 'runs': repeats, 'p50_ms': round(statistics.median(samples), 3),
            'p95_ms': round(samples[int(0.95 * (len(samples) - 1))], 3)}


def scenarios():
    """One constructed case per (rule, status) the rule can give, with the status fixed by construction."""
    P, C, N, R = 'SVC-EXT-PRIMARY', 'SVC-EXT-COMPONENT', 'SVC-EXT-NEVER', 'SVC-EXT-RX'
    auth = {'authorization_id': 'A1', 'patient_id': 'PAT-1', 'service_code': 'SVC-THERAPY', 'status': 'approved',
            'valid_from': '2026-01-01', 'valid_to': '2026-12-31', 'max_quantity': 10}
    prior = claim([line(1, 'SVC-THERAPY', quantity=6, auth='A1'), line(2, 'SVC-LAB', quantity=2)], claim_id='H-0', date='2026-03-01',
                  authorizations=[auth])
    mod = 'EDU-SEPARATE'
    cases = [
        ('E001', 'FAIL', claim([line(1, P), line(2, C)]), []), ('E001', 'PASS', claim([line(1, P), line(2, C, modifier=mod)]), []),
        ('E001', 'NOT_APPLICABLE', claim([line(1, 'SVC-LAB')]), []), ('E001', 'UNABLE_TO_ASSESS', claim([line(1, P), line(2, C, date='soon')]), []),
        ('E002', 'FAIL', claim([line(1, P), line(2, N, modifier=mod)]), []), ('E002', 'PASS', claim([line(1, P), line(2, N)]), []),
        ('E002', 'NOT_APPLICABLE', claim([line(1, P), line(2, C)]), []), ('E002', 'UNABLE_TO_ASSESS', claim([line(1, P), line(2, N, date=None)]), []),
        ('E003', 'FAIL', claim([line(1, 'SVC-LAB')], diagnosis='DX-EXT-ACCIDENT'), []),
        ('E003', 'PASS', claim([line(1, 'SVC-LAB')], diagnosis='DX-EXT-ACCIDENT', notes='Event date: 2026-03-01'), []),
        ('E003', 'NOT_APPLICABLE', claim([line(1, 'SVC-LAB')]), []), ('E003', 'UNABLE_TO_ASSESS', claim([line(1, 'SVC-LAB')], diagnosis=None), []),
        ('E004', 'FAIL', claim([line(1, R)]), []), ('E004', 'PASS', claim([line(1, R, modifier='EDU-ROUTE-IV')]), []),
        ('E004', 'NOT_APPLICABLE', claim([line(1, 'SVC-PHARM')]), []),
        ('E005', 'FAIL', claim([line(1, 'SVC-LAB')], diagnosis='DX-EXT-SECONDARY'), []), ('E005', 'PASS', claim([line(1, 'SVC-LAB')]), []),
        ('E005', 'UNABLE_TO_ASSESS', claim([line(1, 'SVC-LAB')], diagnosis=None), []),
        ('E101', 'FAIL', claim([line(1, 'SVC-LAB', quantity=2)]), [prior]), ('E101', 'PASS', claim([line(1, 'SVC-IMAGE')]), [prior]),
        ('E101', 'UNABLE_TO_ASSESS', claim([line(1, 'SVC-LAB')], provider=None), []),
        ('E102', 'FAIL', claim([line(1, 'SVC-THERAPY', quantity=5, auth='A1')], authorizations=[auth]), [prior]),
        ('E102', 'PASS', claim([line(1, 'SVC-THERAPY', quantity=4, auth='A1')], authorizations=[auth]), [prior]),
        ('E102', 'NOT_APPLICABLE', claim([line(1, 'SVC-LAB')]), []),
        ('E102', 'UNABLE_TO_ASSESS', claim([line(1, 'SVC-THERAPY', auth='A1')], authorizations=[]), []),
        ('E103', 'FAIL', claim([line(1, 'SVC-LAB', quantity=2)]), [prior]), ('E103', 'PASS', claim([line(1, 'SVC-LAB', quantity=1)]), [prior]),
        ('E103', 'NOT_APPLICABLE', claim([line(1, 'SVC-EXT-RX')]), []), ('E103', 'UNABLE_TO_ASSESS', claim([line(1, 'SVC-LAB', date='soon')]), []),
    ]
    results = []
    for rid, want, c, earlier in cases:
        got = {r['rule_id']: r['status'] for r in ex.evaluate_extensions(c, InMemoryHistory(earlier))}[rid]
        results.append({'rule': rid, 'expected': want, 'got': got, 'ok': got == want})
    ok = sum(r['ok'] for r in results)
    bound = clopper_pearson_upper(len(results) - ok, len(results))
    return {'cases': results, 'agree': ok, 'total': len(results), 'disagreement_rate_95pct_upper_bound': round(bound, 4)}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--scale', choices=('tiny', 'full'), default='full')
    ap.add_argument('--out', default=str(ROOT / 'outputs' / 'evaluation' / 'extensions.json'))
    a = ap.parse_args(argv)
    claims = public_claims()
    if a.scale == 'tiny':
        claims = claims[:40]
    counts, agreement = fire_counts(claims)
    big = 200 if a.scale == 'tiny' else 10000
    out = {
        'environment': {'commit': read_git_head(ROOT), 'python': platform.python_version(), 'platform': platform.platform(),
                        'pack_hash': ex.pack_hash(), 'claims': len(claims)},
        'public_claims_status_counts': counts, 'oracle_agreement_on_public_claims': agreement,
        'scenarios': scenarios(),
        'timing': [timing(claims, 0, 30), timing(claims, big, 30)],
        'note': 'Rules E001 to E005 use fictional codes outside the six-code teaching catalogue, so they cannot fire on public data by design; '
                'E101 to E103 need two claims for one patient and every public patient has exactly one.',
    }
    path = Path(a.out)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print('Wrote', path, '| scenarios', out['scenarios']['agree'], '/', out['scenarios']['total'])
    return 0 if out['scenarios']['agree'] == out['scenarios']['total'] else 1


if __name__ == '__main__':
    sys.exit(main())
