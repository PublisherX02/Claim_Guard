"""Mutation check for the work queue: each deliberate breakage of the code must make at least one test fail.

    python tests/mutation_queue.py              # every mutant
    python tests/mutation_queue.py breaker      # only the mutants whose name contains "breaker"

A mutant is a single textual change to one source file (a comparison flipped, a guard removed, a sort reversed). The run happens in a
temporary copy of the repository, so the working tree is never touched. For every mutant the test files that own that code are run; the
mutant is KILLED if any of them fails and SURVIVES if they all pass. A survivor means a behaviour nobody tests: add a test, do not delete
the mutant. A mutant that changes nothing observable (an equivalent mutant) is documented in EQUIVALENT with the reason.

The mutants need the in-memory store only (no MONGO_URI), so the run needs no database.
"""
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PY = sys.executable
COPY = ('src', 'tests', 'rules', 'schemas', 'prompts', 'data', 'scripts')
Q = 'src/workqueue/'

TRIAGE = ['test_wq_triage.py']
DISPATCH = ['test_wq_dispatcher.py', 'test_wq_service.py']
STORE = ['test_wq_store_memory.py']
MINIMIZE = ['test_data_minimization.py']
BREAKER = ['test_wq_breaker.py']
SERVICE = ['test_wq_service.py', 'test_wq_api.py', 'test_wq_signoff.py']
SIGNOFF = ['test_wq_signoff.py']
EXPLAIN = ['test_wq_explain.py']

# (name, file, old, new, test files)
MUTANTS = [
    # ---- triage
    ('triage: lane B needs more than the flagged threshold', Q + 'triage.py', 'len(rows) >= cfg.lane_b_flagged', 'len(rows) > cfg.lane_b_flagged', TRIAGE),
    ('triage: lane B needs more than the score threshold', Q + 'triage.py', "score(results, cfg) >= cfg.lane_b_score", "score(results, cfg) > cfg.lane_b_score", TRIAGE),
    ('triage: a degraded result set may be green', Q + 'triage.py', "    if degraded(results):\n        return 'B'\n", "    if False:\n        return 'B'\n", TRIAGE),
    ('triage: a degraded result set is only decide-level', Q + 'triage.py', "    if degraded(results) or any(_severity(r) == 'high' for r in flagged(results)):", "    if any(_severity(r) == 'high' for r in flagged(results)):", TRIAGE),
    ('triage: an unknown severity counts as medium', Q + 'triage.py', "row.get('severity') if row.get('severity') in KNOWN_SEVERITIES else 'high'", "row.get('severity') if row.get('severity') in KNOWN_SEVERITIES else 'medium'", TRIAGE),
    ('triage: an unable finding scores like a failure', Q + 'triage.py', "return sum(cfg.points[r['status']][_severity(r)] for r in flagged(results))", "return sum(cfg.points['FAIL'][_severity(r)] for r in flagged(results))", TRIAGE),
    # ---- dispatcher
    ('dispatch: the needed permission is not checked', Q + 'dispatcher.py', "ok = [a for a in pool if _needed(d) in a.permissions and room[a.badge_id] > 0 and not _blocked(a, d, excluded)]", "ok = [a for a in pool if room[a.badge_id] > 0 and not _blocked(a, d, excluded)]", DISPATCH),
    ('dispatch: conflicts of interest are not checked', Q + 'dispatcher.py', "room[a.badge_id] > 0 and not _blocked(a, d, excluded)]", "room[a.badge_id] > 0]", DISPATCH),
    ('dispatch: the snapshot forgets who is barred from the patient', Q + 'dispatcher.py', "'excluded_for': sorted({badge for badge, who in cfg.exclusions if who == patient}),", "'excluded_for': [],", DISPATCH + MINIMIZE),
    ('dispatch: the barred list in the snapshot is not honoured', Q + 'dispatcher.py', "\n            or agent.badge_id in doc.get('excluded_for', ()))", ")", DISPATCH + MINIMIZE),
    ('dispatch: capacity is not checked', Q + 'dispatcher.py', "_needed(d) in a.permissions and room[a.badge_id] > 0 and", "_needed(d) in a.permissions and", DISPATCH),
    ('dispatch: the priority sort is reversed', Q + 'dispatcher.py', "order.sort(key=lambda d: -_priority(d, cfg, now))", "order.sort(key=lambda d: _priority(d, cfg, now))", DISPATCH),
    ('dispatch: waiting time is ignored', Q + 'dispatcher.py', "    waited = max(0.0, (now - doc['state_at']) / 3600)\n", "    waited = 0.0\n", DISPATCH),
    ('dispatch: the busiest agent gets the claim', Q + 'dispatcher.py', "pick = min(ok, key=lambda a: (load[a.badge_id], rank[a.badge_id]))", "pick = max(ok, key=lambda a: (load[a.badge_id], rank[a.badge_id]))", DISPATCH),
    ('dispatch: dealt points are not added to the load', Q + 'dispatcher.py', "load[pick.badge_id] += d['receipt']['score']", "load[pick.badge_id] += 0", DISPATCH),
    ('dispatch: a handed-back claim may go straight back', Q + 'dispatcher.py', "        ok = fresh or ok\n", "        ok = ok\n", DISPATCH),
    ('dispatch: agents off shift are dealt to', Q + 'dispatcher.py', "    considered = [a for a in agents if a.active and a.badge_id in cfg.on_shift]\n    zero =", "    considered = [a for a in agents if a.active]\n    zero =", DISPATCH),
    ('dispatch: inactive agents are dealt to', Q + 'dispatcher.py', "    considered = [a for a in agents if a.active and a.badge_id in cfg.on_shift]\n    zero =", "    considered = [a for a in agents if a.badge_id in cfg.on_shift]\n    zero =", DISPATCH),
    ('dispatch: the seed does not shuffle the claims', Q + 'dispatcher.py', "    order = list(ready)\n    rng.shuffle(order)\n", "    order = list(ready)\n", DISPATCH),
    ('dispatch: prior deciders are not remembered', Q + 'dispatcher.py', "            if earlier and earlier.get('decided_by'):", "            if False:", DISPATCH),
    ('dispatch: ineligible holders keep their claims', Q + 'dispatcher.py', "if reason is None and agent is not None and agent.active and need in agent.permissions:", "if True:", DISPATCH),
    ('dispatch: superseded versions are dealt', Q + 'dispatcher.py', "ready = [d for d in pool if newest.get(d['claim_id'], d['version']) <= d['version']]", "ready = list(pool)", DISPATCH + ['test_wq_replay.py']),
    ('dispatch: the scheduled top-up ignores the low-water mark', Q + 'dispatcher.py', "and (full or loads[a.badge_id]['count'] < cfg.low_water)]", "and True]", DISPATCH),
    ('dispatch: expiry hands back claims that are not due', Q + 'dispatcher.py', "        for doc in self.store.expired(now):\n", "        for doc in self.store.by_state('leased', MAX_POOL):\n", DISPATCH),
    # ---- store
    ('store: a transition ignores the expected state', Q + 'store.py', "            if doc is None or doc['state'] != frm:\n                return None\n            if holder", "            if doc is None:\n                return None\n            if holder", STORE),
    ('store: the holder guard is ignored', Q + 'store.py', "            if holder is not None and (not doc['lease'] or doc['lease']['badge_id'] != holder):", "            if False:", STORE),
    ('store: a decision is accepted after the lease ran out', Q + 'store.py', "                    or doc['lease']['expires_at'] <= now:", "                    or False:", STORE),
    ('store: a decision is accepted from another badge', Q + 'store.py', "or not doc['lease'] or doc['lease']['badge_id'] != badge_id \\", "or not doc['lease'] \\", STORE),
    ('store: a lease expires one tick late', Q + 'store.py', "if d['state'] == 'leased' and d['lease']['expires_at'] <= now)", "if d['state'] == 'leased' and d['lease']['expires_at'] < now)", STORE),
    ('store: a configuration with a stale version is stored', Q + 'store.py', "            if current != expected_version:\n                return False\n", "            if False:\n                return False\n", STORE),
    ('store: a counter allows one more than its limit', Q + 'store.py', "            if used >= limit:\n                return False\n", "            if used > limit:\n                return False\n", STORE),
    ('store: a shadow prediction can be overwritten', Q + 'store.py', "            if doc is None or doc.get('shadow') is not None:\n                return False\n", "            if doc is None:\n                return False\n", STORE + ['test_wq_shadow.py']),
    ('store: a duplicate claim is stored again', Q + 'store.py', "                if d['claim_id'] == full['claim_id'] and (d['version'] == full['version'] or (", "                if d['claim_id'] == full['claim_id'] and (False or (", STORE),
    # ---- breaker
    ('breaker: opens one failure late', Q + 'breaker.py', "if self._streak >= self.consecutive or", "if self._streak > self.consecutive or", BREAKER),
    ('breaker: the rate rule ignores the minimum number of calls', Q + 'breaker.py', "(len(self._calls) >= self.min_calls and failures", "(True and failures", BREAKER),
    ('breaker: never leaves the open state', Q + 'breaker.py', "                if now - self._opened_at < self.cooldown:\n                    raise Open('the circuit is open')", "                if True:\n                    raise Open('the circuit is open')", BREAKER),
    ('breaker: old calls never leave the window', Q + 'breaker.py', "now - self._calls[0][0] > self.window", "now - self._calls[0][0] > 10 ** 12", BREAKER),
    ('breaker: fatal errors count as failures', Q + 'breaker.py', "        except Transient:\n            self._record(False, probe)", "        except (Transient, Fatal):\n            self._record(False, probe)", BREAKER),
    ('breaker: several probes at once', Q + 'breaker.py', "                if self._probing:\n                    raise Open('a probe call is already running')", "                if False:\n                    raise Open('a probe call is already running')", BREAKER),
    ('breaker: a failed probe does not reopen', Q + 'breaker.py', "                else:\n                    self._open(now)\n                return", "                else:\n                    self._state = CLOSED\n                return", BREAKER),
    # ---- relay and pipeline
    ('relay: the marker is cleared before the publish', Q + 'relay.py', "        try:\n            publish(", "        store.clear_outbox(doc['claim_id'], doc['version'])\n        try:\n            publish(", ['test_wq_intake.py']),
    ('worker: a message with a stale hash is processed', Q + 'worker.py', "if doc is None or doc['input_hash'] != input_hash:", "if doc is None:", ['test_wq_tasks_eager.py', 'test_wq_faults.py']),
    ('pipeline: a claim past the pipeline is advanced again', Q + 'pipeline.py', "if doc is None or doc['state'] not in PIPELINE_STATES:", "if doc is None:", ['test_wq_pipeline.py', 'test_wq_faults.py']),
    ('reconcile: expired leases are not repaired', Q + 'reconcile.py', "                if lease['expires_at'] <= now:", "                if False:", ['test_wq_reconcile.py']),
    ('intake: a duplicate is stored again', Q + 'intake.py', "doc['input_hash'] == ihash and doc['receipt'].get('rule_pack_hash') == self.rule_pack_hash", "False", ['test_wq_intake.py']),
    # ---- explain
    ('explain: cached text skips the guard', Q + 'explain.py', "            if self.guard(filled, result):\n                return filled, 'cache', 'cache_hit', stopped", "            if True:\n                return filled, 'cache', 'cache_hit', stopped", EXPLAIN),
    ('explain: refused text is cached anyway', Q + 'explain.py', "        if not self.guard(filled, result):\n            return fallback, 'template', 'skipped_guard', stopped\n        self.store.cache_put(key, template)", "        self.store.cache_put(key, template)\n        if not self.guard(filled, result):\n            return fallback, 'template', 'skipped_guard', stopped", EXPLAIN),
    ('explain: the daily budget is not checked', Q + 'explain.py', "if not self.store.bump('ai_day', str(int(now // 86400)), cfg.ai_daily_budget):", "if False:", EXPLAIN),
    ('explain: the rate limit is not checked', Q + 'explain.py', "if not self.store.bump('ai_minute', str(int(now // 60)), cfg.ai_per_minute):", "if False:", EXPLAIN),
    ('explain: the deadline is not enforced after the call', Q + 'explain.py', "            if self.clock() > deadline:\n                return None, 'skipped_timeout'\n            if type(text)", "            if type(text)", EXPLAIN),
    ('explain: the model is told the claim values', Q + 'explain.py', "'placeholders': list(PLACEHOLDERS)}", "'placeholders': list(PLACEHOLDERS), 'value': str((result.get('evidence') or [{}])[0].get('value'))}", EXPLAIN),
    ('explain: fatal errors are retried', Q + 'explain.py', "            except brk.Fatal:\n                return None, 'skipped_error'\n", "            except brk.Fatal:\n                continue\n", EXPLAIN),
    # ---- service
    ('service: the decision uses the token permissions', Q + 'service.py', "    def decide_finding(self, principal, claim_id, rule_id, action, reason):\n        now = self.clock()\n        perms = self._require(principal, 'claims.decide')", "    def decide_finding(self, principal, claim_id, rule_id, action, reason):\n        now = self.clock()\n        perms = principal.permissions", SERVICE),
    ('service: the lease is not checked before a decision', Q + 'service.py', "        doc = self._mine(principal, claim_id, now)\n        sign = doc.get('signoff') or {}", "        doc = self.store.get(claim_id)\n        sign = doc.get('signoff') or {}", SERVICE),
    ('service: a lost lease at write time is ignored', Q + 'service.py', "        if updated is None:\n            raise Conflict('lease_lost')", "        if updated is None:\n            pass", SERVICE),
    ('service: the senior flag is not required for a high finding', Q + 'service.py', "        if _is_high(finding) and 'claims.decide_high' not in perms:\n            raise Forbidden('claims.decide_high')", "        if False:\n            raise Forbidden('claims.decide_high')", SERVICE),
    ('service: a resolved finding can be decided again', Q + 'service.py', "        if _latest_actions(doc, rnd).get(rule_id) in RESOLVING:\n            raise Conflict('already_decided')\n", "", SERVICE),
    ('service: the claim is finished before every finding is resolved', Q + 'service.py', "        if all(latest.get(rid) in RESOLVING for rid in targets):\n            state = self._finish(", "        if any(latest.get(rid) in RESOLVING for rid in targets):\n            state = self._finish(", SERVICE),
    ('service: a person who held the claim before gets 404 not 409', Q + 'service.py', "        if held_before:\n            raise Conflict('lease_lost')\n", "", SERVICE),
    ('service: an expired lease is accepted', Q + 'service.py', "            if lease['expires_at'] <= now:\n                raise Conflict('lease_expired')\n", "", SERVICE),
    ('service: the configuration version is not checked', Q + 'service.py', "        if expected_version != stored_version:\n            raise Conflict('stale_configuration')\n", "", SERVICE),
    ('service: the actions of a resolved finding stay offered', Q + 'service.py', "        if latest.get(finding['rule_id']) in RESOLVING:\n            return None\n", "", SERVICE),
    ('service: the high-severity actions are offered to everyone', Q + 'service.py', "        if finding.get('severity') != 'medium' and 'claims.decide_high' not in perms:\n            return None\n", "", SERVICE),
    # ---- two-person sign-off
    ('signoff: the first signer may countersign', Q + 'service.py', "        if principal.badge in (sign.get('first_by'), sign.get('second_by')):\n            raise Conflict('same_person')", "        if False:\n            raise Conflict('same_person')", SIGNOFF),
    ('signoff: the first signature decides the claim', Q + 'service.py', "            if not high:\n                return self._move(doc, 'decided'", "            if True:\n                return self._move(doc, 'decided'", SIGNOFF),
    ('signoff: a disagreement is treated as agreement', Q + 'service.py', "if all(latest[rid] == sign['actions'][rid] for rid in targets):", "if True:", SIGNOFF),
    ('signoff: one agreeing finding is enough', Q + 'service.py', "if all(latest[rid] == sign['actions'][rid] for rid in targets):", "if any(latest[rid] == sign['actions'][rid] for rid in targets):", SIGNOFF),
    ('signoff: the second round decides every finding again', Q + 'service.py', "    if _stage(doc) is not None:\n        rows = [r for r in rows if _is_high(r)]", "    if False:\n        rows = [r for r in rows if _is_high(r)]", SIGNOFF),
    ('signoff: signers can be dealt the claim again', Q + 'dispatcher.py', "        prior = {b for b in (sign.get('first_by'), sign.get('second_by')) if b}", "        prior = set()", SIGNOFF + DISPATCH),
    ('signoff: a waiting claim needs only the basic permission', Q + 'dispatcher.py', "            if d.get('escalated') or d.get('signoff'):", "            if d.get('escalated'):", SIGNOFF),
    ('signoff: only ready claims can be leased', Q + 'store.py', "            if doc is None or doc['state'] not in DEALABLE:", "            if doc is None or doc['state'] != 'ready':", SIGNOFF + STORE),
    ('signoff: waiting claims are not dealt', Q + 'dispatcher.py', " + self.store.by_state('awaiting_countersign', MAX_POOL)", "", SIGNOFF),
    ('signoff: a disagreement does not escalate the claim', Q + 'service.py', "            state = self._move(doc, 'ready', principal, now,\n                               {'lease': None, 'escalated': True,", "            state = self._move(doc, 'ready', principal, now,\n                               {'lease': None, 'escalated': False,", SIGNOFF),
    ('signoff: the dashboard ignores a missing second senior', Q + 'service.py', "        if any(not [a for a in senior if a.badge_id not in {d['signoff'].get('first_by'), d['signoff'].get('second_by')}] for d in signed):", "        if False:", SIGNOFF),
    # ---- shadow
    ('shadow: a degraded green claim is predicted clear', Q + 'shadow.py', "return 1.0 if receipt.get('lane') == 'green' and not receipt.get('degraded') else 0.0", "return 1.0 if receipt.get('lane') == 'green' else 0.0", ['test_wq_shadow.py']),
    ('shadow: agreement is inverted', Q + 'shadow.py', "agree += (shadow['predicted_clear'] >= 0.5) == human_cleared(doc)", "agree += (shadow['predicted_clear'] >= 0.5) != human_cleared(doc)", ['test_wq_shadow.py']),
    ('shadow: the interval is not exact', Q + 'shadow.py', "lower = 0.0 if k == 0 else _bisect(lambda p: 1.0 - _cdf(k - 1, n, p), alpha / 2, increasing=True)", "lower = 0.0 if k == 0 else max(0.0, k / n - 1.96 * math.sqrt(k / n * (1 - k / n) / n))", ['test_wq_shadow.py']),
]

# Mutants whose change cannot be observed through behaviour, with the reason. None are known yet.
EQUIVALENT = {}


def make_copy():
    tmp = Path(tempfile.mkdtemp(prefix='mutation_queue_'))
    for name in COPY:
        shutil.copytree(ROOT / name, tmp / name, ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
    (tmp / 'outputs' / 'defense').mkdir(parents=True)
    queue_json = ROOT / 'outputs' / 'defense' / 'queue.json'
    if queue_json.exists():
        shutil.copy2(queue_json, tmp / 'outputs' / 'defense' / 'queue.json')
    return tmp


def run_tests(work, files):
    env = {k: v for k, v in os.environ.items() if k not in ('MONGO_URI', 'REDIS_URL', 'REQUIRE_MONGO', 'REQUIRE_REDIS')}
    for name in files:
        r = subprocess.run([PY, '-m', 'unittest', 'discover', '-s', 'tests', '-p', name, '-f'], cwd=work, capture_output=True, text=True,
                           timeout=900, env=env)
        if r.returncode != 0:
            return True, name
    return False, ''


def main(argv):
    only = argv[1:]
    work = make_copy()
    survivors, skipped = [], []
    try:
        baseline_failed, which = run_tests(work, sorted({f for m in MUTANTS for f in m[4]}))
        if baseline_failed:
            print('the unmutated code already fails', which, '- fix that first')
            return 2
        for name, path, old, new, files in MUTANTS:
            if only and not any(o in name for o in only):
                continue
            target = work / path
            original = target.read_bytes()
            text = original.decode('utf-8').replace('\r\n', '\n')
            if text.count(old) != 1:
                print('SKIP (pattern found %d times):' % text.count(old), name, flush=True)
                skipped.append(name)
                continue
            try:
                target.write_bytes(text.replace(old, new, 1).encode('utf-8'))
                killed, by = run_tests(work, files)
            finally:
                target.write_bytes(original)
            print(('killed by %-28s' % by) if killed else 'SURVIVED' + ' ' * 22, name, flush=True)
            if not killed and name not in EQUIVALENT:
                survivors.append(name)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print('mutants: %d  survivors: %d  skipped: %d' % (len(MUTANTS), len(survivors), len(skipped)))
    for s in survivors:
        print('  SURVIVOR:', s)
    for s in skipped:
        print('  SKIPPED:', s)
    return 1 if survivors or skipped else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
