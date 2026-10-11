"""Operational telemetry for the admin console: request metrics, and the health report built from small, independent checks.

Nothing here decides anything about a claim and nothing here stores personal data. Requests are counted by method, route shape and
status (never the query string, a body, a badge or a claim id), so the numbers are safe to show to any administrator and cheap to keep:
one fixed-size ring of one-minute buckets in memory per API process. That is also its limit, stated in the console: it shows this
process since it started, not the whole fleet (the fleet-wide view is the database-backed queue and job records).
"""
import threading
import time
from collections import deque

from audit_log import SECURITY_EVENTS

BUCKET_SECONDS = 60
MAX_BUCKETS = 180                      # three hours of one-minute buckets
LATENCY_EDGES_MS = (5, 10, 25, 50, 100, 250, 500, 1000, 2500)   # the last bucket is "slower than 2.5 s"
MAX_RECENT_ERRORS = 50
EVENT_TYPES_KNOWN = frozenset(SECURITY_EVENTS)      # the audit filter accepts only event types the log can contain
_ID_AFTER = frozenset({'claims', 'users', 'findings'})


def route_shape(path):
    """The route with identifiers collapsed, so /claims/CG-123/findings/R001/decision and any other claim count as one route."""
    parts = [p for p in path.split('?')[0].split('/') if p]
    shaped = []
    for i, part in enumerate(parts):
        shaped.append(':id' if i and parts[i - 1] in _ID_AFTER else part[:40])
    return '/' + '/'.join(shaped[:8])


def _bucket(latency_ms):
    for i, edge in enumerate(LATENCY_EDGES_MS):
        if latency_ms <= edge:
            return i
    return len(LATENCY_EDGES_MS)


def _percentile(counts, q):
    """Upper edge of the latency bucket that contains the q-th fraction of requests (an honest upper bound, not an interpolation)."""
    total = sum(counts)
    if not total:
        return None
    seen = 0
    for i, c in enumerate(counts):
        seen += c
        if seen / total >= q:
            return LATENCY_EDGES_MS[i] if i < len(LATENCY_EDGES_MS) else None
    return None


class Metrics:
    def __init__(self, clock=time.time):
        self.clock = clock
        self.started = clock()
        self._lock = threading.Lock()
        self._buckets = deque(maxlen=MAX_BUCKETS)       # each: {'t', 'n', 'e4', 'e5', 'lat': [counts], 'slow': ms_total}
        self._errors = deque(maxlen=MAX_RECENT_ERRORS)
        self._routes = {}

    def _current(self, now):
        t = int(now // BUCKET_SECONDS) * BUCKET_SECONDS
        if not self._buckets or self._buckets[-1]['t'] != t:
            self._buckets.append({'t': t, 'n': 0, 'e4': 0, 'e5': 0, 'lat': [0] * (len(LATENCY_EDGES_MS) + 1), 'ms': 0.0})
        return self._buckets[-1]

    def record(self, method, path, status, seconds):
        now = self.clock()
        ms = max(0.0, seconds * 1000.0)
        shape = route_shape(path)
        with self._lock:
            b = self._current(now)
            b['n'] += 1
            b['ms'] += ms
            b['lat'][_bucket(ms)] += 1
            if 400 <= status < 500:
                b['e4'] += 1
            elif status >= 500:
                b['e5'] += 1
            key = f'{method} {shape}'
            if key not in self._routes and len(self._routes) >= 200:        # a scanner cannot grow the table without bound
                key = f'{method} /other'
            r = self._routes.setdefault(key, {'n': 0, 'e4': 0, 'e5': 0, 'ms': 0.0, 'lat': [0] * (len(LATENCY_EDGES_MS) + 1)})
            r['n'] += 1
            r['ms'] += ms
            r['lat'][_bucket(ms)] += 1
            if status >= 500:
                r['e5'] += 1
                self._errors.append({'at': now, 'route': key, 'status': status})
            elif status >= 400:
                r['e4'] += 1

    def snapshot(self, minutes=60):
        now = self.clock()
        with self._lock:
            cutoff = (int(now // BUCKET_SECONDS) - minutes + 1) * BUCKET_SECONDS
            series = []
            lat_total = [0] * (len(LATENCY_EDGES_MS) + 1)
            n = e4 = e5 = 0
            by_t = {b['t']: b for b in self._buckets if b['t'] >= cutoff}
            first = int(now // BUCKET_SECONDS) * BUCKET_SECONDS - (minutes - 1) * BUCKET_SECONDS
            for k in range(minutes):
                t = first + k * BUCKET_SECONDS
                b = by_t.get(t)
                if b:
                    series.append({'t': t, 'n': b['n'], 'e4': b['e4'], 'e5': b['e5'], 'p95_ms': _percentile(b['lat'], 0.95)})
                    n += b['n']; e4 += b['e4']; e5 += b['e5']
                    lat_total = [x + y for x, y in zip(lat_total, b['lat'])]
                else:
                    series.append({'t': t, 'n': 0, 'e4': 0, 'e5': 0, 'p95_ms': None})
            routes = [{'route': k, 'requests': v['n'], 'client_errors': v['e4'], 'server_errors': v['e5'],
                       'mean_ms': round(v['ms'] / v['n'], 2) if v['n'] else None, 'p95_ms': _percentile(v['lat'], 0.95)}
                      for k, v in sorted(self._routes.items(), key=lambda kv: -kv[1]['n'])[:15]]
            return {
                'uptime_seconds': round(now - self.started, 1), 'window_minutes': minutes, 'requests': n, 'client_errors': e4, 'server_errors': e5,
                'p50_ms': _percentile(lat_total, 0.5), 'p95_ms': _percentile(lat_total, 0.95), 'p99_ms': _percentile(lat_total, 0.99),
                'latency_edges_ms': list(LATENCY_EDGES_MS), 'latency_counts': lat_total, 'series': series, 'routes': routes,
                'recent_errors': list(self._errors)[-10:][::-1],
            }


class MetricsLayer:
    """Outermost ASGI layer: times every HTTP request and records its final status. It sees the 4xx/5xx the inner layers produce."""

    def __init__(self, app, metrics):
        self.app, self.metrics = app, metrics

    async def __call__(self, scope, receive, send):
        if scope['type'] != 'http':
            return await self.app(scope, receive, send)
        start = time.perf_counter()
        status = 500

        async def tracking_send(message):
            nonlocal status
            if message['type'] == 'http.response.start':
                status = message['status']
            await send(message)
        try:
            await self.app(scope, receive, tracking_send)
        finally:
            self.metrics.record(scope.get('method', '?'), scope.get('path', '/'), status, time.perf_counter() - start)


# ---- health -----------------------------------------------------------------------------------------------------------
OK, DEGRADED, DOWN, UNKNOWN = 'ok', 'degraded', 'down', 'unknown'
_RANK = {OK: 0, UNKNOWN: 1, DEGRADED: 2, DOWN: 3}
MAX_DETAIL = 240


def worst(statuses):
    return max(statuses, key=lambda s: _RANK[s], default=OK)


def check(name, label, fn, clock=time.time, weight='core'):
    """Run one probe. fn() returns (status, detail, extra) or raises; a probe that raises is reported as down, never as an error.

    `weight` says whether the system can still work without it: 'core' components make the whole report degraded/down, 'optional'
    ones (the AI helper, the RFC 3161 timestamp) can be down while claims are still reviewed.
    """
    t = time.perf_counter()
    try:
        status, detail, extra = fn()
    except Exception as e:                       # noqa: BLE001 -- a broken probe is itself the finding
        status, detail, extra = DOWN, f'check failed: {type(e).__name__}', {}
    return {'id': name, 'label': label, 'status': status, 'detail': str(detail)[:MAX_DETAIL], 'weight': weight,
            'latency_ms': round((time.perf_counter() - t) * 1000, 1), 'checked_at': clock(), **extra}


def overall(components):
    """The headline: a core component that is down makes the system down; any other problem makes it degraded.
    A component that has not reported yet (unknown) is shown as such but does not by itself change the headline."""
    core = worst(c['status'] for c in components if c['weight'] == 'core')
    if core == DOWN:
        return DOWN
    anything = worst(c['status'] for c in components)
    return DEGRADED if anything in (DEGRADED, DOWN) else OK


# ---- audit: what happened, to whom, when -------------------------------------------------------------------------------
FAILURE_EVENTS = frozenset({'login_failure', 'lockout', 'forbidden', 'token_rejected'})
MAX_SCAN_ROWS = 50_000
HOUR = 3600


def _parse_time(text):
    from datetime import datetime
    try:
        return datetime.fromisoformat(text).timestamp()
    except (TypeError, ValueError):
        return None


def event_actor(event):
    return event.get('actor') or event.get('badge_id')


def matching(rows, event_type=None, badge=None, claim_id=None, since=None, until=None):
    """Filter audit rows ({sequence, recorded_at, event}). Every filter is an exact match except the time bounds (epoch seconds)."""
    out = []
    for r in rows:
        e = r['event']
        if event_type and e.get('event_type') != event_type:
            continue
        if badge and event_actor(e) != badge and e.get('badge_id') != badge:
            continue
        if claim_id and e.get('claim_id') != claim_id:
            continue
        if since is not None or until is not None:
            t = _parse_time(r['recorded_at'])
            if t is None or (since is not None and t < since) or (until is not None and t > until):
                continue
        out.append(r)
    return out


def audit_summary(rows, now, hours=24):
    """Counts for the audit panel: by type, per hour for the last `hours`, the busiest actors and the failure signals."""
    first = (int(now // HOUR) - hours + 1) * HOUR
    series = {first + k * HOUR: {'t': first + k * HOUR, 'events': 0, 'failures': 0} for k in range(hours)}
    by_type, actors = {}, {}
    for r in rows:
        e = r['event']
        kind = e.get('event_type', '?')
        by_type[kind] = by_type.get(kind, 0) + 1
        t = _parse_time(r['recorded_at'])
        if t is not None:
            slot = series.get(int(t // HOUR) * HOUR)
            if slot:
                slot['events'] += 1
                slot['failures'] += kind in FAILURE_EVENTS
                who = event_actor(e)
                if who:
                    actors[who] = actors.get(who, 0) + 1
    failures = {k: by_type.get(k, 0) for k in sorted(FAILURE_EVENTS)}
    return {'total': len(rows), 'window_hours': hours, 'by_type': dict(sorted(by_type.items(), key=lambda kv: -kv[1])),
            'series': list(series.values()), 'actors': [{'badge': b, 'events': n} for b, n in sorted(actors.items(), key=lambda kv: -kv[1])[:10]],
            'failures': failures, 'first_at': rows[0]['recorded_at'] if rows else None, 'last_at': rows[-1]['recorded_at'] if rows else None}


class AuditStatus:
    """The last result of verifying both hash-chained logs, remembered so the health report does not re-verify on every refresh."""

    def __init__(self, verify, clock=time.time):
        self._verify, self._clock = verify, clock
        self._lock = threading.Lock()
        self.last = None

    def refresh(self):
        result = self._verify()
        with self._lock:
            self.last = {'at': self._clock(), **result}
        return self.last

    def get(self, max_age=300):
        with self._lock:
            last = self.last
        if last is None or self._clock() - last['at'] > max_age:
            return self.refresh()
        return last


# ---- health probes of the access side ----------------------------------------------------------------------------------
def access_probes(service, securitylog, audit_status, data_dir, metrics, clock=time.time):
    import shutil

    def api_probe():
        snap = metrics.snapshot(15)
        reqs, errs = snap['requests'], snap['server_errors']
        extra = {'uptime_seconds': snap['uptime_seconds']}
        if reqs >= 20 and errs / reqs > 0.10:
            return DOWN, f'{errs} of {reqs} requests failed in the last 15 minutes', extra
        if errs:
            return DEGRADED, f'{errs} server errors in the last 15 minutes ({reqs} requests)', extra
        return OK, f'up {int(snap["uptime_seconds"])} s; {reqs} requests in the last 15 minutes, no server errors', extra

    def users_probe():
        return (OK, 'user database answers a ping', {}) if service.store.ping() else (DOWN, 'the user database does not answer', {})

    def audit_probe():
        last = audit_status.get()
        sec, rev = last['security'], last['review']
        extra = {'security_events': sec.get('events', 0), 'review_events': rev.get('events', 0), 'verified_at': last['at']}
        if sec['ok'] and rev['ok']:
            return OK, f"both logs verified: {extra['security_events']} security and {extra['review_events']} review events, chains intact", extra
        bad = [n for n, r in (('security', sec), ('review', rev)) if not r['ok']]
        return DOWN, f"the {' and '.join(bad)} log failed verification: treat the history as untrusted until explained", extra

    def anchor_probe():
        from audit_log import anchor_status
        s = anchor_status(securitylog.path)
        if not s['key_configured']:
            return DEGRADED, 'no anchor key is configured, so tampering by whole-file replacement could go unnoticed', s
        if not s['anchor_signed']:
            return DEGRADED, 'the log anchor is not yet signed', s
        return OK, 'the log head is anchored and signed', s

    def stamp_probe():
        import timestamp_anchor
        st = timestamp_anchor.timestamp_status(securitylog.path, securitylog.path.with_name(securitylog.path.name + '.head.json'))
        state = st.get('state')
        if state == 'current':
            return OK, 'an RFC 3161 timestamp covers the whole log', st
        if state == 'none':
            return UNKNOWN, 'no external timestamp (optional: stamp the log with a Time-Stamp Authority)', st
        if state == 'stale':
            return OK, f"timestamp valid; {st.get('unstamped_events', '?')} newer events are not stamped yet", st
        return DEGRADED, f'timestamp state: {state}', st

    def disk_probe():
        u = shutil.disk_usage(data_dir)
        free = u.free / u.total
        extra = {'free_bytes': u.free, 'total_bytes': u.total}
        if free < 0.02:
            return DOWN, f'{free:.0%} of the disk is free: audit logs can no longer be written', extra
        if free < 0.10:
            return DEGRADED, f'{free:.0%} of the disk is free', extra
        return OK, f'{free:.0%} of the disk is free ({u.free // 2**30} GB)', extra

    def signals_probe():
        rows = securitylog.scan(MAX_SCAN_ROWS)
        cutoff = clock() - HOUR
        recent = [r for r in rows if (_parse_time(r['recorded_at']) or 0) >= cutoff]
        counts = {k: sum(1 for r in recent if r['event'].get('event_type') == k) for k in sorted(FAILURE_EVENTS)}
        extra = {'last_hour': counts}
        if counts['lockout']:
            return DEGRADED, f"{counts['lockout']} account lockout(s) in the last hour", extra
        if counts['login_failure'] >= 20:
            return DEGRADED, f"{counts['login_failure']} failed logins in the last hour", extra
        return OK, f"{counts['login_failure']} failed logins, {counts['forbidden']} refused requests in the last hour", extra

    return [('api', 'Reviewer API', 'core', api_probe), ('user_store', 'User database', 'core', users_probe),
            ('audit_logs', 'Audit logs (hash chains)', 'core', audit_probe), ('disk', 'Disk space for the logs', 'core', disk_probe),
            ('anchor', 'Log anchor (tamper evidence)', 'core', anchor_probe), ('timestamp', 'External timestamp (RFC 3161)', 'optional', stamp_probe),
            ('signals', 'Security signals (last hour)', 'optional', signals_probe)]


def build_report(probes, metrics, clock=time.time):
    """Run every probe (each one isolated), and put the request metrics beside them."""
    components = [check(name, label, fn, clock, weight) for name, label, weight, fn in probes]
    return {'overall': overall(components), 'checked_at': clock(), 'components': components, 'metrics': metrics.snapshot(60)}

