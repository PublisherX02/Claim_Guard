"""Heartbeats for the scheduled jobs (deal, expire, relay, reconcile), so the health panel can tell a stopped worker from a quiet one.

The record lives in the queue database, not in a process, because the worker that runs the jobs is a different process from the API
that reports on them. Recording never breaks a job: if the record cannot be written, the job's own result still stands.
"""
EXPECTED_EVERY = {'relay': 10.0, 'deal': 15.0, 'expire': 60.0, 'reconcile': 300.0}   # seconds, from the Celery beat schedule


def run_job(store, clock, name, fn):
    """Run fn(), remember when it ran and whether it worked, return its result (or raise its error unchanged)."""
    try:
        result = fn()
    except Exception as e:
        _record(store, clock, name, False, type(e).__name__)
        raise
    _record(store, clock, name, True, '' if result is None else str(result))
    return result


def _record(store, clock, name, ok, detail):
    try:
        store.record_job(name, clock(), ok, detail[:200])
    except Exception:  # noqa: BLE001 -- telemetry must never turn a working job into a failed one
        pass


def job_status(jobs, now, grace=3.0):
    """Judge each expected job: ok when it succeeded within `grace` times its interval; unknown when it never ran."""
    by_name = {j['name']: j for j in jobs}
    out = []
    for name, every in EXPECTED_EVERY.items():
        j = by_name.get(name)
        if j is None:
            out.append({'name': name, 'status': 'unknown', 'detail': 'has not run yet', 'every_seconds': every})
            continue
        age = now - j['at']
        if not j['ok']:
            status, detail = 'degraded', f"last run failed ({j['detail'] or 'error'}) {int(age)} s ago"
        elif age > every * grace:
            status, detail = 'down', f'last ran {int(age)} s ago, expected every {int(every)} s'
        else:
            status, detail = 'ok', f'ran {int(age)} s ago'
        out.append({'name': name, 'status': status, 'detail': detail, 'every_seconds': every, 'last_at': j['at'], 'runs': j['runs'], 'failures': j['failures']})
    return out
