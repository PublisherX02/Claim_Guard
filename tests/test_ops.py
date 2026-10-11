"""The admin console's health and audit features: request metrics, health probes, job heartbeats, audit filters and summaries, claim trace.

Unit tests for the pure parts, then the HTTP routes (permission matrix, nothing sensitive in the answers) against every queue store.
"""
import json
import sys
import time
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import ops
from wq_api_world import QueueWorld
from wq_world import store_makers
from workqueue import health, jobs
from workqueue.breaker import CircuitBreaker

API = '/api/v1'


class FakeClock:
    def __init__(self, t=1_000_000.0):
        self.t = t

    def __call__(self):
        return self.t


class RouteShapeTests(unittest.TestCase):
    def test_identifiers_collapse_so_one_claim_is_not_one_route(self):
        self.assertEqual(ops.route_shape('/api/v1/claims/CG-123/findings/R001/decision'), '/api/v1/claims/:id/findings/:id/decision')
        self.assertEqual(ops.route_shape('/api/v1/users/CG-2002/unlock'), '/api/v1/users/:id/unlock')
        self.assertEqual(ops.route_shape('/api/v1/work/claims/CG-9/verify'), '/api/v1/work/claims/:id/verify')

    def test_query_strings_and_long_paths_are_cut(self):
        self.assertEqual(ops.route_shape('/api/v1/audit/events?badge=CG-1&claim_id=SECRET'), '/api/v1/audit/events')
        self.assertLessEqual(len(ops.route_shape('/' + '/'.join('a' * 100 for _ in range(30)))), 8 * 41 + 1)


class MetricsTests(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        self.m = ops.Metrics(self.clock)

    def test_counts_statuses_and_latency_percentiles(self):
        for _ in range(90):
            self.m.record('GET', '/api/v1/queue/dashboard', 200, 0.004)
        for _ in range(10):
            self.m.record('GET', '/api/v1/queue/dashboard', 200, 0.4)
        self.m.record('GET', '/api/v1/x', 404, 0.001)
        self.m.record('POST', '/api/v1/y', 500, 0.001)
        s = self.m.snapshot(5)
        self.assertEqual((s['requests'], s['client_errors'], s['server_errors']), (102, 1, 1))
        self.assertEqual(s['p50_ms'], 5)          # the upper edge of the bucket holding the median
        self.assertEqual(s['p99_ms'], 500)
        self.assertEqual(s['recent_errors'][0]['status'], 500)

    def test_series_has_one_slot_per_minute_with_empty_minutes_present(self):
        self.m.record('GET', '/a', 200, 0.001)
        self.clock.t += 180
        self.m.record('GET', '/a', 200, 0.001)
        series = self.m.snapshot(5)['series']
        self.assertEqual(len(series), 5)
        self.assertEqual(sum(p['n'] for p in series), 2)
        self.assertEqual(sum(1 for p in series if p['n'] == 0), 3)

    def test_old_minutes_fall_out_of_the_window(self):
        self.m.record('GET', '/a', 200, 0.001)
        self.clock.t += 3600
        self.assertEqual(self.m.snapshot(10)['requests'], 0)

    def test_the_route_table_cannot_grow_without_bound(self):
        for i in range(500):
            self.m.record('GET', f'/scan{i}', 404, 0.001)
        self.assertLessEqual(len(self.m._routes), 201)

    def test_nothing_personal_is_kept(self):
        self.m.record('GET', '/api/v1/claims/PAT-SECRET-77/findings/R1', 200, 0.001)
        self.assertNotIn('SECRET', json.dumps(self.m.snapshot(5)))

    def test_the_error_ring_is_bounded(self):
        for _ in range(200):
            self.m.record('GET', '/a', 500, 0.001)
        self.assertLessEqual(len(self.m._errors), ops.MAX_RECENT_ERRORS)


class HealthAssemblyTests(unittest.TestCase):
    def test_a_probe_that_raises_is_reported_as_down_not_as_an_error(self):
        c = ops.check('x', 'X', lambda: 1 / 0)
        self.assertEqual(c['status'], ops.DOWN)
        self.assertIn('ZeroDivisionError', c['detail'])

    def test_long_details_are_cut(self):
        self.assertLessEqual(len(ops.check('x', 'X', lambda: (ops.OK, 'a' * 5000, {}))['detail']), ops.MAX_DETAIL)

    def test_a_core_failure_is_down_and_an_optional_one_only_degrades(self):
        core_down = [{'status': ops.DOWN, 'weight': 'core'}, {'status': ops.OK, 'weight': 'optional'}]
        optional_down = [{'status': ops.OK, 'weight': 'core'}, {'status': ops.DOWN, 'weight': 'optional'}]
        degraded = [{'status': ops.DEGRADED, 'weight': 'core'}]
        unknown = [{'status': ops.UNKNOWN, 'weight': 'core'}, {'status': ops.OK, 'weight': 'core'}]
        self.assertEqual(ops.overall(core_down), ops.DOWN)
        self.assertEqual(ops.overall(optional_down), ops.DEGRADED)
        self.assertEqual(ops.overall(degraded), ops.DEGRADED)
        self.assertEqual(ops.overall(unknown), ops.OK)
        self.assertEqual(ops.overall([]), ops.OK)

    def test_build_report_runs_every_probe_independently(self):
        probes = [('a', 'A', 'core', lambda: (ops.OK, 'fine', {})), ('b', 'B', 'core', lambda: 1 / 0), ('c', 'C', 'optional', lambda: (ops.OK, 'fine', {}))]
        report = ops.build_report(probes, ops.Metrics(FakeClock()), FakeClock())
        self.assertEqual([c['status'] for c in report['components']], [ops.OK, ops.DOWN, ops.OK])
        self.assertEqual(report['overall'], ops.DOWN)


class JobStatusTests(unittest.TestCase):
    def jobs_at(self, ages, ok=True):
        now = 10_000.0
        return now, [{'name': n, 'at': now - age, 'ok': ok, 'detail': 'boom' if not ok else '', 'runs': 5, 'failures': 0 if ok else 5} for n, age in ages.items()]

    def test_nothing_recorded_is_unknown_not_down(self):
        out = jobs.job_status([], 5.0)
        self.assertEqual({j['status'] for j in out}, {'unknown'})
        self.assertEqual({j['name'] for j in out}, set(jobs.EXPECTED_EVERY))

    def test_on_time_late_and_failed(self):
        now, rows = self.jobs_at({'relay': 5, 'deal': 100, 'expire': 30, 'reconcile': 100})
        by = {j['name']: j['status'] for j in jobs.job_status(rows, now)}
        self.assertEqual(by, {'relay': 'ok', 'deal': 'down', 'expire': 'ok', 'reconcile': 'ok'})
        now, rows = self.jobs_at({'relay': 5}, ok=False)
        self.assertEqual({j['name']: j['status'] for j in jobs.job_status(rows, now)}['relay'], 'degraded')

    def test_run_job_records_success_and_failure_and_never_hides_the_error(self):
        from workqueue.store import MemoryQueueStore
        store, clock = MemoryQueueStore(), FakeClock(50.0)
        self.assertEqual(jobs.run_job(store, clock, 'relay', lambda: 3), 3)
        with self.assertRaises(ZeroDivisionError):
            jobs.run_job(store, clock, 'deal', lambda: 1 / 0)
        by = {j['name']: j for j in store.jobs()}
        self.assertTrue(by['relay']['ok'])
        self.assertEqual((by['deal']['ok'], by['deal']['detail']), (False, 'ZeroDivisionError'))

    def test_a_broken_store_does_not_break_the_job(self):
        class Broken:
            def record_job(self, *a, **k):
                raise RuntimeError('db down')
        self.assertEqual(jobs.run_job(Broken(), FakeClock(), 'relay', lambda: 'done'), 'done')


def row(seq, when, **event):
    return {'sequence': seq, 'recorded_at': datetime.fromtimestamp(when, timezone.utc).isoformat(), 'event': event}


class AuditHelperTests(unittest.TestCase):
    NOW = 1_000_000.0

    def rows(self):
        return [row(1, self.NOW - 7200, event_type='login_success', badge_id='CG-2002'),
                row(2, self.NOW - 3000, event_type='login_failure', reason='bad'),
                row(3, self.NOW - 2000, event_type='decision', badge_id='CG-2002', claim_id='C1', rule_id='R001', action='confirm_issue'),
                row(4, self.NOW - 1000, event_type='forbidden', badge_id='CG-1001', method='GET', path='/x'),
                row(5, self.NOW - 10, event_type='user_updated', actor='CG-4004', badge_id='CG-2002', changes=['level'])]

    def test_filters_are_exact_and_combine(self):
        r = self.rows()
        self.assertEqual([x['sequence'] for x in ops.matching(r, event_type='decision')], [3])
        self.assertEqual([x['sequence'] for x in ops.matching(r, badge='CG-2002')], [1, 3, 5])
        self.assertEqual([x['sequence'] for x in ops.matching(r, claim_id='C1')], [3])
        self.assertEqual([x['sequence'] for x in ops.matching(r, badge='CG-2002', event_type='decision')], [3])
        self.assertEqual([x['sequence'] for x in ops.matching(r, since=self.NOW - 2500)], [3, 4, 5])
        self.assertEqual([x['sequence'] for x in ops.matching(r, until=self.NOW - 2500)], [1, 2])
        self.assertEqual(ops.matching(r, claim_id='C'), [])           # exact match, not a prefix

    def test_a_row_with_a_garbled_time_never_matches_a_time_filter_and_never_crashes(self):
        bad = [{'sequence': 1, 'recorded_at': 'not a time', 'event': {'event_type': 'logout'}}]
        self.assertEqual(ops.matching(bad, since=0), [])
        self.assertEqual(ops.audit_summary(bad, self.NOW)['total'], 1)

    def test_summary_counts_hours_actors_and_failures(self):
        s = ops.audit_summary(self.rows(), self.NOW, hours=3)
        self.assertEqual(len(s['series']), 3)
        self.assertEqual(s['total'], 5)
        self.assertEqual(s['failures']['login_failure'], 1)
        self.assertEqual(s['failures']['forbidden'], 1)
        self.assertEqual(s['by_type']['decision'], 1)
        self.assertEqual({a['badge'] for a in s['actors']}, {'CG-2002', 'CG-1001', 'CG-4004'})
        self.assertEqual(sum(p['failures'] for p in s['series']), 2)

    def test_summary_of_nothing(self):
        s = ops.audit_summary([], self.NOW)
        self.assertEqual((s['total'], s['first_at'], s['actors']), (0, None, []))

    def test_the_status_cache_does_not_re_verify_on_every_call(self):
        calls = []
        clock = FakeClock()
        status = ops.AuditStatus(lambda: calls.append(1) or {'security': {'ok': True}, 'review': {'ok': True}}, clock)
        status.get(); status.get()
        self.assertEqual(len(calls), 1)
        clock.t += 301
        status.get()
        self.assertEqual(len(calls), 2)
        status.refresh()
        self.assertEqual(len(calls), 3)


class OpsApiBase:
    def setUp(self):
        self.qstore, self.cleanup = self.factory()
        self.w = QueueWorld(self.qstore)
        self.w.staff()
        self.w.on_shift('CG-2002', 'CG-3003', slice_size=4, low_water=1)
        self.breaker = CircuitBreaker(self.w.clock)
        self.stack = SimpleNamespace(store=self.qstore, service=self.w.queue, explain=SimpleNamespace(breaker=self.breaker, model_name='none'))
        self.probes = health.queue_probes(self.stack, {}, self.w.clock)
        self.w.app = self.w.build_app(extra_probes=self.probes)

    def tearDown(self):
        self.w.close()
        self.cleanup()

    def admin(self):
        return self.w.login_client(badge='CG-4004')

    def components(self, report):
        return {c['id']: c for c in report['components']}

    # ---- who may call what
    def test_permission_matrix_of_the_new_routes(self):
        rows = [('health', f'{API}/ops/health', {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
                ('audit summary', f'{API}/ops/audit/summary', {None: 401, 1: 403, 2: 403, 3: 403, 4: 200}),
                ('audit filtered', f'{API}/audit/events?event_type=login_success&newest_first=true', {None: 401, 1: 403, 2: 403, 3: 403, 4: 200})]
        badge = {1: 'CG-1001', 2: 'CG-2002', 3: 'CG-3003', 4: 'CG-4004'}
        failures = []
        for name, path, expected in rows:
            for caller in (None, 1, 2, 3, 4):
                client = self.w.client() if caller is None else self.w.login_client(badge=badge[caller])
                got = client.get(path).status_code
                if got != expected[caller]:
                    failures.append(f'{name} as {caller}: expected {expected[caller]}, got {got}')
        self.assertEqual(failures, [])

    def test_a_refused_health_request_is_audited(self):
        self.w.login_client(badge='CG-2002').get(f'{API}/ops/health')
        self.assertIn(('CG-2002', 'GET', f'{API}/ops/health'),
                      [(e['badge_id'], e['method'], e['path']) for e in self.w.events() if e['event_type'] == 'forbidden'])

    # ---- the health report
    def test_the_report_names_every_expected_component(self):
        report = self.admin().get(f'{API}/ops/health').json()
        self.assertEqual(set(self.components(report)),
                         {'api', 'user_store', 'audit_logs', 'disk', 'anchor', 'timestamp', 'signals', 'queue_store', 'broker', 'scheduler', 'outbox', 'flow', 'model'})
        for c in report['components']:
            self.assertIn(c['status'], ('ok', 'degraded', 'down', 'unknown'))
            self.assertIn(c['weight'], ('core', 'optional'))
        self.assertEqual(report['components'][0]['id'], 'api')
        self.assertIn('metrics', report)

    def test_a_fresh_system_is_not_down_and_the_scheduler_is_unknown(self):
        report = self.admin().get(f'{API}/ops/health').json()
        by = self.components(report)
        self.assertEqual(by['scheduler']['status'], 'unknown')
        self.assertEqual(by['queue_store']['status'], 'ok')
        self.assertEqual(by['audit_logs']['status'], 'ok')
        self.assertNotEqual(report['overall'], 'down')

    def test_a_stopped_worker_turns_the_report_down(self):
        now = self.w.clock()
        for name in jobs.EXPECTED_EVERY:
            self.qstore.record_job(name, now - 5000, True, '')
        report = self.admin().get(f'{API}/ops/health').json()
        self.assertEqual(self.components(report)['scheduler']['status'], 'down')
        self.assertEqual(report['overall'], 'down')

    def test_running_jobs_make_the_scheduler_ok(self):
        admin = self.admin()                              # logging in moves the test clock on
        now = self.w.clock()
        for name in jobs.EXPECTED_EVERY:
            self.qstore.record_job(name, now - 1, True, 'fine')
        self.assertEqual(self.components(admin.get(f'{API}/ops/health').json())['scheduler']['status'], 'ok')

    def test_an_open_breaker_degrades_only_the_optional_model_check(self):
        self.stack.explain.model_name = 'gemma3:4b'
        for _ in range(6):
            try:
                self.breaker.call(lambda: (_ for _ in ()).throw(__import__('workqueue.breaker', fromlist=['Transient']).Transient('x')))
            except Exception:
                pass
        report = self.admin().get(f'{API}/ops/health').json()
        self.assertEqual(self.components(report)['model']['status'], 'degraded')
        self.assertEqual(self.components(report)['model']['weight'], 'optional')

    def test_dead_letters_and_stuck_publishing_are_reported(self):
        self.qstore.add_dead_letter({'dead_id': 'X1', 'claim_id': 'C1', 'reason': 'boom', 'at': self.w.clock()})
        cid = self.w.find('green')
        claim, _ = self.w.claims.get(cid)
        self.w.intake.submit(claim)                       # stays in the outbox: nothing published it
        self.w.clock.advance(health.STUCK_OUTBOX_SECONDS + 10)
        by = self.components(self.admin().get(f'{API}/ops/health').json())
        self.assertEqual(by['flow']['status'], 'degraded')
        self.assertIn('dead-letter', by['flow']['detail'])
        self.assertEqual(by['outbox']['status'], 'degraded')

    def test_a_tampered_audit_log_is_reported_down(self):
        admin = self.admin()
        path = self.w.log.path
        text = path.read_text(encoding='utf-8')
        first = json.loads(text.split('\n')[0])
        first['event']['badge_id'] = 'FORGED'
        path.write_text(json.dumps(first) + '\n' + '\n'.join(text.split('\n')[1:]), encoding='utf-8')
        report = admin.get(f'{API}/ops/health').json()
        self.assertEqual(self.components(report)['audit_logs']['status'], 'down')
        self.assertEqual(report['overall'], 'down')

    def test_the_report_carries_no_secret_and_no_claim_value(self):
        cid = self.w.ready(self.w.find('medium'))
        text = self.admin().get(f'{API}/ops/health').text
        claim, _ = self.w.claims.get(cid)
        for needle in (claim['patient_id'], claim['member_id'], claim['invoice_number'], 'password', 'FERNET', 'Traceback'):
            self.assertNotIn(needle, text)

    def test_requests_are_counted_by_the_metrics_layer(self):
        self.w.client().get(f'{API}/ops/health')                     # a 401
        report = self.admin().get(f'{API}/ops/health').json()
        m = report['metrics']
        self.assertGreaterEqual(m['requests'], 2)
        self.assertGreaterEqual(m['client_errors'], 1)
        self.assertTrue(any(r['route'] == f'GET {API}/ops/health' for r in m['routes']))

    # ---- the audit panel
    def test_audit_summary_and_filters_over_http(self):
        admin = self.admin()
        self.w.login_client(badge='CG-2002')
        s = admin.get(f'{API}/ops/audit/summary?hours=6').json()
        self.assertEqual(len(s['series']), 6)
        self.assertGreaterEqual(s['by_type']['login_success'], 2)
        r = admin.get(f'{API}/audit/events?event_type=login_success&badge=CG-2002&newest_first=true').json()
        self.assertTrue(r['events'])
        self.assertTrue(all(e['event']['event_type'] == 'login_success' and e['event']['badge_id'] == 'CG-2002' for e in r['events']))
        self.assertEqual(r['matched'], len(r['events']))
        seqs = [e['sequence'] for e in r['events']]
        self.assertEqual(seqs, sorted(seqs, reverse=True))

    def test_an_unknown_event_type_filter_is_refused(self):
        self.assertEqual(self.admin().get(f'{API}/audit/events?event_type=nonsense').status_code, 422)
        self.assertEqual(self.admin().get(f'{API}/audit/events?since=abc').status_code, 422)

    def test_the_unfiltered_audit_route_still_behaves_as_before(self):
        r = self.admin().get(f'{API}/audit/events?limit=3').json()
        self.assertEqual(set(r), {'events'})
        self.assertLessEqual(len(r['events']), 3)

    # ---- the claim trace
    def test_trace_shows_who_did_what_and_no_claim_values(self):
        cid = self.w.ready(self.w.find('medium'))
        self.w.lease_to(cid, 'CG-2002')
        rule = [r['rule_id'] for r in self.w.claims.get(cid)[1] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS')][0]
        client = self.w.login_client(badge='CG-2002')
        self.assertEqual(client.post(f'{API}/work/claims/{cid}/findings/{rule}/decision',
                                     json={'action': 'confirm_issue', 'reason': 'checked'}).status_code, 200)
        r = self.admin().get(f'{API}/ops/trace/{cid}')
        self.assertEqual(r.status_code, 200)
        t = r.json()
        self.assertEqual(t['claim_id'], cid)
        v = t['versions'][0]
        self.assertEqual(v['version'], 1)
        self.assertEqual([e['to'] for e in v['events']][:2], ['triaged', 'ready'])
        self.assertEqual([d['actor'] for d in v['decisions']], ['CG-2002'])
        self.assertIn('rule_pack_hash', v['receipt'])
        self.assertIn(rule, [f['rule_id'] for f in v['findings']])
        self.assertTrue(any(e['event']['event_type'] == 'decision' for e in t['security_log']))
        claim, _ = self.w.claims.get(cid)
        for needle in (claim['patient_id'], claim['member_id'], claim['invoice_number']):
            self.assertNotIn(needle, r.text)

    def test_trace_of_an_unknown_claim_is_404_and_needs_audit_permission(self):
        self.assertEqual(self.admin().get(f'{API}/ops/trace/NOPE').status_code, 404)
        cid = self.w.ready(self.w.find('green'))
        for badge, code in (('CG-1001', 403), ('CG-2002', 403), ('CG-3003', 403), ('CG-4004', 200)):
            self.assertEqual(self.w.login_client(badge=badge).get(f'{API}/ops/trace/{cid}').status_code, code, badge)
        self.assertEqual(self.w.client().get(f'{API}/ops/trace/{cid}').status_code, 401)


def _make(name, factory):
    return type('OpsApi_' + name, (OpsApiBase, unittest.TestCase), {'factory': staticmethod(factory)})


for _name, _factory in store_makers():
    globals()['OpsApi_' + _name] = _make(_name, _factory)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
