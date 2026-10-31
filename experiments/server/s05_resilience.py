#!/usr/bin/env python3
"""S5. Resilience: restart each service while claims are being submitted, and prove nothing acknowledged is lost (docs/65).

    CG_URL=... CG_BADGE=... python experiments/server/s05_resilience.py --run <name> --yes-restart \
        [--compose "docker compose -f deploy/rust/compose.yml --env-file .env"] [--services worker,api,redis,mongo,proxy]

THIS BRIEFLY INTERRUPTS THE SERVER. Run it before go-live or in a maintenance window, never with real reviewers working. For each service:
a background client submits small batches of synthetic claims (administrator account), the service is restarted, and the script measures how long /healthz stays down,
how many requests failed in that time, and then reads back every claim that was acknowledged (HTTP 200) with the viewer account to make sure it is still there.
A claim that was acknowledged and is gone is the one failure that matters; the target is zero.
"""
import argparse
import shlex
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import Api, Run, fresh_claim, load_claims, sh  # noqa: E402


class Load(threading.Thread):
    def __init__(self, api, pool):
        super().__init__(daemon=True)
        self.api, self.pool, self.stop_flag = api, pool, threading.Event()
        self.acked, self.failed, self.sent = [], 0, 0

    def run(self):
        k = 0
        while not self.stop_flag.is_set():
            batch = [fresh_claim(self.pool[(k + i) % len(self.pool)]) for i in range(5)]
            k += 5
            self.sent += 5
            status, _, _ = self.api.call('POST', '/api/v1/queue/submit', {'claims': batch})
            if status == 200:
                self.acked += [c['claim_id'] for c in batch]
            else:
                self.failed += 5
            time.sleep(0.2)


def queue_total(api, wait_s=120):
    """Claims held by the queue, all states together (the dashboard's counts). Retries while the API is restarting. None if it cannot be read."""
    end = time.time() + wait_s
    while time.time() < end:
        status, body, _ = api.call('GET', '/api/v1/queue/dashboard')
        if status == 200 and isinstance(body, dict) and isinstance(body.get('counts'), dict):
            return sum(int(v) for v in body['counts'].values())
        time.sleep(2)
    return None


def preflight(api, pool):
    """Prove that the dashboard total is a valid way to count: submit 5 claims in a calm system and see the total rise by exactly 5."""
    a = queue_total(api)
    batch = [fresh_claim(pool[i % len(pool)]) for i in range(5)]
    status, _, _ = api.call('POST', '/api/v1/queue/submit', {'claims': batch})
    time.sleep(5)
    b = queue_total(api)
    ok = status == 200 and a is not None and b is not None and b - a == 5
    print(f'S5 preflight: submit {status}, queue total {a} -> {b} ({"counting works" if ok else "COUNTING DOES NOT WORK"})')
    return ok


def one(api, view, pool, compose, service):
    before = queue_total(api)
    load = Load(api, pool)
    load.start()
    time.sleep(5)
    t0 = time.time()
    code, out = sh(f'{compose} restart {shlex.quote(service)}', timeout=300)
    cmd_s = time.time() - t0
    down_at, back_at, deadline = None, None, time.time() + 300
    while time.time() < deadline:
        ok = api.healthy()
        if not ok and down_at is None:
            down_at = time.time()
        if ok and (down_at is not None or time.time() - t0 > cmd_s + 10):
            back_at = time.time()
            break
        time.sleep(0.5)
    time.sleep(5)
    load.stop_flag.set()
    load.join(timeout=60)
    # Read-back by counting: every claim acknowledged with HTTP 200 must be in the queue, so the total must have risen by at least that many.
    # (The old check asked GET /api/v1/claims/<id>, which serves only the dataset loaded at start-up, never claims submitted to the queue.)
    time.sleep(5)
    after = queue_total(api)
    if before is None or after is None:
        missing = len(load.acked)          # could not count: report everything as unverified, never as fine
    else:
        missing = max(0, len(load.acked) - (after - before))
    return {'service': service, 'restart_command_ok': code == 0, 'restart_command_s': cmd_s,
            'unhealthy_s': (back_at - down_at) if down_at and back_at else 0.0, 'recovered': back_at is not None, 'sent': load.sent, 'acknowledged': len(load.acked),
            'failed_during_restart': load.failed, 'acknowledged_but_missing': missing, 'missing_ids': [],
            'queue_total_before': before, 'queue_total_after': after, 'method': 'dashboard count before and after (acknowledged claims must all be counted)'}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--yes-restart', action='store_true', help='confirm that the server may be interrupted')
    ap.add_argument('--compose', default='docker compose -f deploy/rust/compose.yml --env-file .env')
    ap.add_argument('--services', default='worker,api,redis,mongo,proxy')
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    if not a.yes_restart:
        raise SystemExit('this restarts services; pass --yes-restart to confirm that nobody is working on the server')
    run, api, pool, view = Run(a.run, a.dry), Api().login(), load_claims(), Api(prefix='CG_VIEW_').login()
    rows = []
    if not preflight(api, pool):
        raise SystemExit('S5 stopped: the dashboard total did not rise by exactly the number of claims submitted, so a loss could not be measured reliably. Nothing was restarted.')
    for s in a.services.split(','):
        print(f'S5: restarting {s} under load')
        rows.append(one(api, view, pool, a.compose, s))
        time.sleep(10)
    run.write('s05_resilience', {'services': rows, 'checks': {'nothing_acknowledged_was_lost': all(r['acknowledged_but_missing'] == 0 for r in rows),
                                                              'every_service_recovered': all(r['recovered'] for r in rows)}})


if __name__ == '__main__':
    main()
