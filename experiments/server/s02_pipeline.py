#!/usr/bin/env python3
"""S2. Claim pipeline throughput through the real API (proxy, API, queue, engine, store), and an optional soak (docs/65).

    CG_URL=... CG_BADGE=... CG_PASSWORD=... CG_TOTP_SEED=... python experiments/server/s02_pipeline.py --run <name>
        [--stages 10x6,50x6,200x4,500x2] [--soak-minutes 0] [--soak-rate 100]

A stage is `SIZExREPEATS`: that many batches of that many claims, one after another, each submitted asynchronously (POST /api/v1/queue/submit with
"async": true) and followed until the job is done. Per batch it records the time to accept the batch and the time until every claim is in the
queue. Claims are the synthetic pack of data/stress/ under fresh ids, so every run is new work. Before and after the stages it times three read
requests (the queue dashboard, the claims list, the health panel), so a server that is still busy or has slowed down shows up as a slower answer. The soak submits batches of 10 at a steady rate for N minutes and records
claims per minute and the batch time for each minute (memory over the same time comes from the sampler, S0).
"""
import argparse
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import Api, Run, fresh_claim, load_claims, summary  # noqa: E402


def submit_batch(api, claims):
    """(accepted seconds, finished seconds or None, number of rows, error or None)."""
    t0 = time.perf_counter()
    status, body, dt_accept = api.call('POST', '/api/v1/queue/submit', {'claims': claims, 'async': True})
    if status != 202:
        return dt_accept, None, 0, f'submit {status} {str(body)[:100]}'
    job = body['job_id']
    while time.perf_counter() - t0 < 1800:
        status, body, _ = api.call('GET', f'/api/v1/queue/submit/{job}')
        if status != 200:
            return dt_accept, None, 0, f'job {status}'
        if body.get('state') == 'done':
            return dt_accept, time.perf_counter() - t0, len(body.get('results') or []), None
        if body.get('state') not in ('running', 'done'):
            return dt_accept, None, 0, f"job {body.get('state')}: {body.get('error')}"
        time.sleep(0.25)
    return dt_accept, None, 0, 'job timed out after 30 minutes'


def responsiveness(api, view, n=10):
    """Seconds of three cheap read requests, ten times each: how quickly the server answers while it is idle or just after a load. Only answers with
    status 200 are timed (a refusal is fast and would flatter the result); the claims list is read with the viewer account when one is given."""
    out = {}
    for name, who, path in (('dashboard', api, '/api/v1/queue/dashboard'), ('claims_list', view, '/api/v1/claims?limit=5'), ('health_panel', api, '/api/v1/ops/health')):
        if who is None:
            continue
        got = [who.call('GET', path) for _ in range(n)]
        out[name] = {**summary([dt for s, _, dt in got if s == 200]), 'not_ok': sum(1 for s, _, _ in got if s != 200)}
    return out


def stage(api, pool, size, repeats):
    rows, errors, t_start, taken = [], [], time.perf_counter(), 0
    for _ in range(repeats):
        batch = [fresh_claim(pool[(taken + i) % len(pool)]) for i in range(size)]
        taken += size
        accept, done, n, err = submit_batch(api, batch)
        rows.append({'accept_s': accept, 'done_s': done, 'claims': n})
        if err:
            errors.append(err)
    wall = time.perf_counter() - t_start
    ok = [r for r in rows if r['done_s'] is not None]
    return {'batch_size': size, 'batches': repeats, 'wall_s': wall, 'claims_ok': sum(r['claims'] for r in ok), 'errors': errors[:5], 'error_count': len(errors),
            'claims_per_minute': sum(r['claims'] for r in ok) / wall * 60 if wall else 0, 'accept_s': summary([r['accept_s'] for r in rows]),
            'done_s': summary([r['done_s'] for r in ok]), 'per_claim_ms': summary([r['done_s'] / r['claims'] * 1000 for r in ok if r['claims']])}


def soak(api, pool, minutes, rate):
    batch_size = 10
    gap = 60.0 * batch_size / rate
    per_minute, t_end, k = {}, time.time() + minutes * 60, 0
    t_begin = time.time()
    while time.time() < t_end:
        t0 = time.time()
        batch = [fresh_claim(pool[(k + i) % len(pool)]) for i in range(batch_size)]
        k += batch_size
        _, done, n, err = submit_batch(api, batch)
        m = per_minute.setdefault(int((t0 - t_begin) // 60), {'claims': 0, 'errors': 0, 'done_s': []})
        m['claims'] += n
        m['errors'] += 1 if err else 0
        if done is not None:
            m['done_s'].append(done)
        time.sleep(max(0.0, gap - (time.time() - t0)))
    return [{'minute': k, 'claims': v['claims'], 'errors': v['errors'], 'batch_done_p50_s': summary(v['done_s']).get('p50'), 'batch_done_p95_s': summary(v['done_s']).get('p95')}
            for k, v in sorted(per_minute.items())]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--stages', default='10x6,50x6,200x4,500x2')
    ap.add_argument('--soak-minutes', type=float, default=0)
    ap.add_argument('--soak-rate', type=float, default=100, help='claims per minute for the soak')
    ap.add_argument('--claims', default='')
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run, api, pool = Run(a.run, a.dry), Api().login(), load_claims(a.claims or None)
    view = Api(prefix='CG_VIEW_').login() if os.environ.get('CG_VIEW_BADGE') else None
    result = {'stages': [], 'soak': [], 'claims_available': len(pool), 'responsive_before': responsiveness(api, view)}
    for spec in a.stages.split(',') if a.stages else []:
        size, rep = (int(x) for x in spec.split('x'))
        print(f'S2: stage {size} claims x {rep}')
        result['stages'].append(stage(api, pool, size, rep))
    result['responsive_after'] = responsiveness(api, view)
    if a.soak_minutes > 0:
        print(f'S2: soak {a.soak_minutes} min at {a.soak_rate} claims/min')
        result['soak'] = soak(api, pool, a.soak_minutes, a.soak_rate)
    best = max((s['claims_per_minute'] for s in result['stages']), default=0)
    result['checks'] = {'best_claims_per_minute': best, 'reaches_200_per_minute': best >= 200,
                        'no_errors': all(s['error_count'] == 0 for s in result['stages']) and all(m['errors'] == 0 for m in result['soak']),
                        'still_responsive_after_load': all((result['responsive_after'][k].get('p95') or 0) < 2.0 and not result['responsive_after'][k].get('not_ok') for k in result['responsive_after'])}
    run.write('s02_pipeline', result)


if __name__ == '__main__':
    main()
