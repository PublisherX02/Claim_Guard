"""The queue's share of the health report: store, broker, scheduled jobs, outbox, claim flow and the AI helper's circuit breaker.

Every probe is a small function returning (status, detail, extra). Nothing here changes any state, and nothing returns a claim value,
an identifier or a secret: only counts, ages and states.
"""
from access import ops

from . import jobs as jobs_mod

STUCK_OUTBOX_SECONDS = 120          # a claim that has waited this long to be published means the relay or the broker is not working
STUCK_LEASE_SECONDS = 180           # a lapsed lease the expiry job has not yet returned
OLD_WAIT_SECONDS = 1800


def queue_probes(stack, env, clock, demo=False):
    """The list of (id, label, weight, fn) the report runs for a queue deployment."""
    store, service, step = stack.store, stack.service, stack.explain

    def store_probe():
        return (ops.OK, 'answers a ping', {}) if store.ping() else (ops.DOWN, 'the queue database does not answer', {})

    def broker_probe():
        if demo:
            return ops.OK, 'in-process (demo mode, no broker)', {}
        url = (env.get('REDIS_URL') or '').strip()
        if not url:
            return ops.UNKNOWN, 'REDIS_URL is not set in this process', {}
        import redis
        client = redis.Redis.from_url(url, socket_connect_timeout=1, socket_timeout=1)
        try:
            return (ops.OK, 'Redis answers a ping', {}) if client.ping() else (ops.DOWN, 'Redis did not answer', {})
        finally:
            client.close()

    def jobs_probe():
        statuses = jobs_mod.job_status(store.jobs(), clock())
        worst = ops.worst(s['status'] for s in statuses)
        if worst == ops.OK:
            detail = 'all four scheduled jobs are running on time'
        else:
            detail = '; '.join(f"{s['name']}: {s['detail']}" for s in statuses if s['status'] != ops.OK)
        return worst, detail, {'jobs': statuses}

    def outbox_probe():
        pending = store.pending_outbox(500)
        now = clock()
        oldest = max((now - d['receipt']['created_at'] for d in pending), default=0.0)
        extra = {'pending': len(pending), 'oldest_seconds': round(oldest, 1)}
        if pending and oldest > STUCK_OUTBOX_SECONDS:
            return ops.DEGRADED, f'{len(pending)} claims wait to be published, the oldest for {int(oldest)} s: the relay or the broker is stuck', extra
        return ops.OK, f'{len(pending)} waiting to be published', extra

    def flow_probe():
        now = clock()
        counts = store.counts()
        dead = store.dead_letters(200)
        lapsed = [d for d in store.expired(now) if now - d['lease']['expires_at'] > STUCK_LEASE_SECONDS]
        dash = service.dashboard_snapshot()
        extra = {'dead_letters': len(dead), 'lapsed_leases': len(lapsed), 'waiting': dash['waiting'], 'oldest_waiting_seconds': dash['oldest_waiting_seconds'],
                 'shortages': dash['shortages'], 'counts': counts}
        problems = []
        if dead:
            problems.append(f'{len(dead)} claims in the dead-letter list')
        if lapsed:
            problems.append(f'{len(lapsed)} lapsed leases not yet returned')
        problems.extend(dash['shortages'])
        if max(dash['oldest_waiting_seconds'].values()) > OLD_WAIT_SECONDS:
            problems.append('a claim has waited more than 30 minutes')
        return (ops.DEGRADED, '; '.join(problems), extra) if problems else (ops.OK, 'claims are flowing, nothing is stuck', extra)

    def model_probe():
        state = step.breaker.state
        name = getattr(step, 'model_name', 'none')
        if name == 'none':
            return ops.OK, 'no model configured: explanations use the built-in templates', {'model': name, 'breaker': state}
        if state == 'closed':
            return ops.OK, f'{name}: breaker closed', {'model': name, 'breaker': state}
        return ops.DEGRADED, f'{name}: breaker {state}; explanations fall back to templates, nothing is blocked', {'model': name, 'breaker': state}

    return [('queue_store', 'Queue database', 'core', store_probe), ('broker', 'Message broker (Redis)', 'core', broker_probe),
            ('scheduler', 'Scheduled jobs and workers', 'core', jobs_probe), ('outbox', 'Publishing of new claims', 'core', outbox_probe),
            ('flow', 'Claim flow', 'core', flow_probe), ('model', 'AI explanation helper', 'optional', model_probe)]
