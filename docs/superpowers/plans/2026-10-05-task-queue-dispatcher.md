# Task Queue and Work Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every claim is checked, given a triage receipt and a lane, explained by the AI where useful, and dealt by a dispatcher into the personal inbox of an eligible human agent, with every step recorded, replayable and reconcilable and no claim ever cleared without a human.

**Architecture:** A new package `src/workqueue/` holds a pure Python core (triage, states, dispatcher, breaker, explain, replay, reconcile) behind a `QueueStore` interface with an in-memory twin and a MongoDB implementation, both proven by one contract suite. Celery (`tasks.py`) is a thin adapter over the core, Redis is only its broker, MongoDB is the single source of truth, and intake is crash-safe through a one-document atomic write plus an outbox relay. The existing access layer (clearance, permissions, hide-not-disable API, security log) is reused, not changed, except for two new level-4 permissions.

**Tech Stack:** Python 3.10 to 3.14; `celery==5.6.3`, `redis==8.1.0` (new, runtime); existing `pymongo`, `fastapi`, `bcrypt`, `PyJWT`; `unittest`, Hypothesis; MongoDB 7 and Redis 7 in Docker.

**Spec:** `docs/superpowers/specs/2026-10-04-task-queue-dispatcher-design.md`

## Naming decision that overrides the spec text

The spec says `src/queue/`. **Do not use that name**: `src/` is put on `sys.path` by the tests and scripts, so a package called `queue` would shadow Python's standard `queue` module and break `concurrent.futures`, `logging.handlers` and Celery itself. The package is `src/workqueue/` everywhere. Update the spec's one mention in Task 13.

## Global Constraints

- The offline engine, the evaluation and all existing tests (1019 now) must keep passing and must not import `src/workqueue`, `celery` or `redis`.
- No claim is ever cleared, closed or decided without a human badge recorded on the decision. Shadow mode only stores a prediction and has no effect on routing.
- Permissions, sessions and clearance are never cached; they are read from the store on every request and again at decision time.
- `src/` and `scripts/` must not contain the sink words of `tests/security_sinks.py` (`subprocess`, `pickle`, `marshal`, `shelve`, `os.system`, `os.popen`, bare `eval(`, `exec(`, `compile(`, `input(`, `__import__(`), not even in docstrings. `bandit -r src scripts -ll` stays clean.
- Every store method refuses non-string identifiers up front (a dict such as `{"$ne": null}` must never reach a query), exactly as `src/access/store.py` does. Any driver failure except a duplicate key becomes `StoreUnavailable`; callers refuse (fail closed).
- Lane and routing numbers (points 4/2/2/1, lane B at 4 flagged or score 10, slice 25, low-water 10, lease 1800 s, circuit breaker 5 failures or 50 % over 60 s with at least 20 calls, 90 s ceiling) are defaults held in the routing configuration, never constants in code paths.
- Run single modules with `python -m unittest discover -s tests -p <file>` and `.venv/Scripts/python.exe`. Mongo tests read `MONGO_URI`, Redis tests read `REDIS_URL`; with `REQUIRE_MONGO=1` / `REQUIRE_REDIS=1` (set in CI) a missing URL is a failure, otherwise a loud skip.
- Commits end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` and the `Claude-Session:` line. Do not push without being asked. Stage files by explicit path and show `git add` errors; `outputs/` needs `git add -f`.
- Never name a temp script `dis.py` or any stdlib module name. Write patch scripts to the scratchpad with the Write tool.

## Review Focus

Failure modes the spec implies but no obvious test would exercise; each gets a pinned test in the owning task.

1. **A claim whose results are missing, short (not 15), or carry an unknown severity must never land in green.** It fails closed to lane B, eligibility `decide_high`, receipt flag `degraded: true` (Task 2).
2. **A demotion or deactivation while an agent holds a lease**: the lease confers no rights; permission is re-read from the store at decision time and a stripped agent gets 403 and the claim returns to the pool (Tasks 7 and 10).
3. **Decision racing lease expiry**: an agent deciding in the same instant the lease is reassigned must either win (lease still theirs) or get a 409 with no write; never two decisions, never a lost update (Tasks 4, 7, 10).
4. **No eligible agent on shift** (nobody on shift, no L3 for a high claim, everybody at capacity): claims stay `ready`, nothing is assigned to an ineligible agent, the dashboard shows the shortage and nothing crashes (Tasks 7 and 10).
5. **Duplicate and out-of-order delivery**: the same task delivered twice, a task arriving for a claim already past that state, and a crash between the atomic write and the publish: no claim lost, none processed twice, the outbox relay republishes (Tasks 6 and 9).
6. **A routing configuration change mid-deal** or an invalid one (negative points, slice below low-water, lease of zero): the deal uses one config version end to end, and an invalid config is refused whole (Tasks 1 and 7).

---

### Task 1: Foundations: dependencies, package, permissions, routing configuration

**Files:** Create `src/workqueue/__init__.py`, `src/workqueue/routing_config.py`, `tests/test_wq_routing_config.py`. Modify `requirements.txt`, `requirements-dev.txt` if needed, `src/access/permissions.py`, `tests/test_access_permissions.py` and any test that lists the permission tuple literally (find with `Grep "claims.decide_high"` in `tests/`), `src/audit_log.py` (`SECURITY_EVENTS`).

**Interfaces (produces):**
- `permissions.PERMISSIONS` gains `'routing.manage'` and `'queue.view'`; both are added to `SENSITIVE` (so they can only come from level 4) and to `_L4`. L1 to L3 do not get them.
- `SECURITY_EVENTS` gains `'routing_config_changed': {'actor', 'version', 'before', 'after'}`, `'lease_expired': {'claim_id', 'badge_id'}`, `'claim_dealt': {'deal_id', 'claim_id', 'badge_id'}`, `'claim_decided_green': {'badge_id', 'claim_id', 'action'}`.
- `routing_config.RoutingConfig` frozen dataclass: `version: int = 1`, `points: dict` default `{'FAIL': {'high': 4, 'medium': 2}, 'UNABLE_TO_ASSESS': {'high': 2, 'medium': 1}}`, `lane_b_flagged: int = 4`, `lane_b_score: int = 10`, `slice_size: int = 25`, `low_water: int = 10`, `lease_seconds: int = 1800`, `aging_per_hour: float = 0.5`, `ai_per_minute: int = 30`, `ai_daily_budget: int = 2000`, `on_shift: tuple = ()`, `exclusions: tuple = ()` (pairs `(badge_id, patient_id)`).
- `routing_config.validate(cfg) -> RoutingConfig` raises `ValueError` unless: all point values are ints 0 to 100 and FAIL points >= UNABLE points per severity; `1 <= lane_b_flagged <= 15`; `1 <= lane_b_score <= 1000`; `1 <= low_water <= slice_size <= 200`; `60 <= lease_seconds <= 86400`; `0 <= aging_per_hour <= 100`; `0 <= ai_per_minute <= 600`; `0 <= ai_daily_budget <= 100000`; `on_shift` is a tuple of non-empty str with no duplicates; each exclusion is a pair of non-empty str.
- `routing_config.to_doc(cfg) -> dict` and `routing_config.from_doc(doc) -> RoutingConfig` (round trip exact; `from_doc` validates; unknown keys raise).
- `routing_config.DEFAULT = validate(RoutingConfig())`.

- [ ] **Step 1: Failing tests** `test_wq_routing_config.py`: defaults equal the spec table literally (written out, not derived); every invalid case above raises (one assertion per bound, both sides); `to_doc`/`from_doc` round trip; unknown key raises; a dict for `on_shift` raises; `True` is not accepted as an int. In `test_access_permissions.py` update the literal level tables: L4 gains the two flags and L1 to L3 do not; `check_user_change` refuses granting `routing.manage` to an L2.
- [ ] **Step 2: Run, RED** (`ModuleNotFoundError: workqueue`; permission literals fail).
- [ ] **Step 3: Implement** `routing_config.py`, the two permission edits and the four event types. Add the two pins to `requirements.txt` with a comment; `.venv/Scripts/python.exe -m pip install -r requirements-dev.txt`.
- [ ] **Step 4: GREEN**, then run the whole suite once (`python -m unittest discover -s tests`) and `test_security_owasp.py`. Commit `feat(queue): package, routing configuration and level-4 queue permissions`.

---

### Task 2: Triage: formula, receipt and the independent oracle

**Files:** Create `src/workqueue/triage.py`, `tests/oracle_routing.py`, `tests/test_wq_triage.py`.

**Interfaces:**
- Consumes: `RoutingConfig` (Task 1); rule results as produced by `yara_engine.evaluate(claim, cfg, [])` (dicts with `rule_id`, `status`, `severity`, `affected_line_ids`).
- Produces: `triage.input_hash(claim) -> str` (SHA-256 hex of canonical JSON: sorted keys, no spaces, `ensure_ascii=False`); `triage.result_hash(results) -> str` (same, over results sorted by `rule_id`); `triage.flagged(results) -> list[dict]` (status in `FAIL`, `UNABLE_TO_ASSESS`); `triage.score(results, cfg) -> int`; `triage.lane(results, cfg) -> 'green'|'A'|'B'`; `triage.eligibility(results) -> 'decide'|'decide_high'`; `triage.make_receipt(claim, results, cfg, rule_pack_hash, engine_version, now) -> dict` with keys `claim_id, input_hash, rule_pack_hash, engine_version, facts_hash, result_hash, statuses (rule_id -> status), score, lane, eligibility, config_version, degraded, created_at`.

Rule (fail closed): a result set is **degraded** when it is not a list of exactly 15 dicts, has a duplicate or missing `rule_id`, an unknown `status`, or an unknown `severity`. A degraded set is lane `B`, eligibility `decide_high`, `degraded: true`, and an unknown severity counts as `high`. Never green.

```python
def lane(results, cfg):
    rows = flagged(results)
    if degraded(results):
        return 'B'
    if not rows:
        return 'green'
    if len(rows) >= cfg.lane_b_flagged or score(results, cfg) >= cfg.lane_b_score:
        return 'B'
    return 'A'
```

- [ ] **Step 1: Write `tests/oracle_routing.py`**, an independent implementation that does not import `triage`: it recomputes points from the table with plain loops and returns `(score, lane, eligibility, degraded)`.
- [ ] **Step 2: Failing tests** `test_wq_triage.py`: table cases written out (one FAIL high = 4 points lane A; two FAIL high + one FAIL medium = 10 points lane B by score; four UNABLE medium = 4 flagged lane B; one UNABLE medium = 1 point lane A; none flagged green; a PASS and NOT_APPLICABLE never count); eligibility: any flagged high finding means `decide_high`, a flagged medium only means `decide`, green means `decide`; **Review Focus 1**: empty list, 14 results, 16 results, duplicate `R001`, `status: 'MAYBE'`, `severity: 'urgent'`, a non-dict item, `None` all give lane `B`, `decide_high`, `degraded True`; Hypothesis (200 examples) over generated 15-result sets with every status and severity checks `triage` equals the oracle on score, lane, eligibility; the public evaluation sets (`data/` splits through `FileClaimStore.from_jsonl`) agree with the oracle on every claim and the lane counts are printed; `make_receipt` is deterministic for equal inputs, and changes `result_hash` when one status changes, `input_hash` when one field changes.
- [ ] **Step 3: RED. Step 4: Implement. Step 5: GREEN**, sink scan, commit `feat(queue): triage formula, receipt and independent routing oracle`.

---

### Task 3: State machine

**Files:** Create `src/workqueue/states.py`, `tests/test_wq_states.py`.

**Interfaces (produces):** `states.STATES = ('received','triaged','explained','explanation_skipped','ready','leased','decided','rechecked','dead_lettered')`; `states.TRANSITIONS: dict[str, frozenset[str]]`:

```python
TRANSITIONS = {
    'received': {'triaged', 'dead_lettered'},
    'triaged': {'explained', 'explanation_skipped', 'ready', 'dead_lettered'},   # green goes straight to ready
    'explained': {'ready', 'dead_lettered'},
    'explanation_skipped': {'ready', 'dead_lettered'},
    'ready': {'leased', 'dead_lettered'},
    'leased': {'ready', 'decided', 'dead_lettered'},          # ready again on lease expiry
    'decided': {'rechecked'},
    'rechecked': {'triaged'},
    'dead_lettered': {'triaged'},                             # an administrator replay
}
```

`states.check_transition(frm, to) -> None` raises `IllegalTransition(ValueError)`; `states.make_event(frm, to, actor, now, detail=None) -> dict` with `from, to, actor, at, detail` (`actor` is a badge or `system:<worker>`; `detail` is a small dict of text, numbers and booleans only, validated like `SecurityLog` values).

- [ ] **Step 1: Failing tests:** every pair of the 9x9 table checked against the literal dict (allowed pairs pass, all others raise); `decided` can only go to `rechecked`; unknown state names, non-str, `None` raise; `make_event` rejects a nested object deeper than 3, a value that is not text/number/bool/list/dict, an actor that is empty or not a string; Hypothesis: any random walk of allowed transitions from `received` stays inside `STATES`.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN, commit** `feat(queue): claim state machine`.

---

### Task 4: Store interface, in-memory twin and the contract suite

**Files:** Create `src/workqueue/store.py`, `tests/queue_store_contract.py`, `tests/test_wq_store_memory.py`.

**Interfaces (produces).** One claim version is one document:
`{claim_id, version, input_hash, claim, results, receipt, state, state_at, enqueue_pending, lease: None | {badge_id, leased_at, expires_at, heartbeat_at}, events: [event...], decided_by: None | str, shadow: None | dict}`.
Unique on `(claim_id, version)`; unique on `(claim_id, input_hash)`; the latest version of a claim is the highest `version`.

`QueueStore` methods (every id argument checked with `check_text`; every returned document is a deep copy):
- `put_triaged(doc) -> bool`: atomic insert of the whole document in state `triaged` with `enqueue_pending: True` and its first events; returns `False` (no change) if `(claim_id, input_hash)` already exists.
- `get(claim_id, version=None) -> dict | None` (latest when `version` is None).
- `transition(claim_id, version, frm, to, actor, now, detail=None, set_fields=None) -> dict | None`: one atomic conditional update: only if the stored state is `frm`; sets `state`, `state_at`, appends the event, applies `set_fields` (allowed names only: `lease`, `decided_by`, `shadow`, `explanation`, `enqueue_pending`); returns the new document or `None` when the state was not `frm`. Calls `states.check_transition` first.
- `pending_outbox(limit) -> list[dict]` (documents with `enqueue_pending` true, oldest first); `clear_outbox(claim_id, version) -> bool`.
- `by_state(state, limit=1000) -> list[dict]`; `counts() -> dict` keyed `(state, lane, eligibility)` as `'state|lane|eligibility'` strings -> int.
- `lease(claim_id, version, badge_id, now, expires_at) -> dict | None`: conditional `ready -> leased` setting `lease`; `None` if not `ready`.
- `inbox(badge_id) -> list[dict]` (state `leased`, `lease.badge_id == badge_id`, oldest lease first); `heartbeat(badge_id, now, expires_at) -> int` (extends every lease of that badge; returns how many).
- `expired(now) -> list[dict]` (state `leased`, `lease.expires_at <= now`).
- `claims_for_patient(patient_id) -> list[dict]` (latest version of each claim of that patient, claim body only fields `claim_id, patient_id, provider_id, submission_date, lines, authorizations, notes, diagnosis_code, attachments`).
- `put_config(doc, expected_version) -> bool` (atomic: stores the next numbered configuration only when the current latest version equals `expected_version`); `latest_config() -> dict | None`; `config_history() -> list[dict]`.
- `append_deal(doc) -> None`; `get_deal(deal_id) -> dict | None`; `deals(limit) -> list`.
- `add_dead_letter(doc) -> None`; `dead_letters(limit=100) -> list`; `pop_dead_letter(dead_id) -> dict | None`.
- `cache_get(key) -> str | None`; `cache_put(key, text) -> None`.
- `bump(counter, window_key, limit) -> bool` (atomic: increments the named counter for that window and returns `False` without incrementing once `limit` is reached).

The in-memory twin takes a `threading.RLock` around every method so it behaves like the atomic Mongo operations.

- [ ] **Step 1: Write `tests/queue_store_contract.py`** (a `StoreContract` mixin with `make_store()`; the memory test class and, in Task 5, the Mongo class both subclass it). Cases: `put_triaged` stores and `get` returns equal copies; mutating a returned document changes nothing stored; a second `put_triaged` with the same `(claim_id, input_hash)` returns `False`; a new `input_hash` for the same claim is version 2 only through `put_triaged` with `version=2`; `transition` from the right state works and appends exactly one event, from the wrong state returns `None` and changes nothing; **Review Focus 3**: 50 threads race `lease` on one `ready` claim, exactly one wins; 50 threads race `transition(leased -> decided)` against `transition(leased -> ready)`, exactly one wins and the document ends consistent; `lease` on a non-ready claim is `None`; `heartbeat` extends only that badge; `expired` respects `now` exactly at the boundary (`<=`); `put_config` with a stale `expected_version` returns `False` and with 20 parallel writers at the same expected version exactly one wins; `bump` allows exactly `limit` increments across 50 threads and a new window key starts at zero; dead letter round trip and `pop` once only; cache round trip; **every method refuses** `{'$ne': None}`, `None`, a list and an int as an identifier with `TypeError`; `counts()` matches a hand-counted fixture; `set_fields` with an unknown name raises `ValueError`.
- [ ] **Step 2: RED. Step 3: Implement `store.py`** (`QueueStore` Protocol, `StoreUnavailable` and `check_text` re-exported from `access.store`, `MemoryQueueStore`). **Step 4: GREEN, commit** `feat(queue): store interface, in-memory twin and contract suite`.

---

### Task 5: MongoDB store

**Files:** Create `src/workqueue/store_mongo.py`, `tests/test_wq_store_mongo.py`. Modify `docker-compose.yml` (add `redis:7` bound to `127.0.0.1:6379`, healthcheck `redis-cli ping`, no persistence needed).

**Interfaces:** `MongoQueueStore(uri, db_name, timeout_ms=3000)` implements `QueueStore`; `MongoQueueStore.ensure_indexes()`; `MongoHistory(store)` with `earlier_claims(claim) -> list[dict]` using `claims_for_patient` and `claim_history.order_key` so it returns exactly what `InMemoryHistory.earlier_claims` returns for the same data.

Guarantees come from the database, in the manner of `src/access/store_mongo.py`: unique indexes on `(claim_id, version)` and `(claim_id, input_hash)`; every transition and lease is one `find_one_and_update` filtered on the expected `state` (and for leases on `lease.expires_at`); config versions are a unique index on `version`; `bump` is one upsert with `$inc` guarded by `{count: {$lt: limit}}`; the cache and counters get TTL indexes. Driver failures other than `DuplicateKeyError` raise `StoreUnavailable`.

- [ ] **Step 1: Failing test** `test_wq_store_mongo.py` copying the layout of `tests/test_access_store_mongo.py`: `MongoIsConfigured` (fails under `REQUIRE_MONGO=1` with no `MONGO_URI`), a class subclassing `StoreContract` with a fresh database name per test (`uuid`) dropped in `tearDown`, plus `MongoHistoryTests`: the same fixture claims through `InMemoryHistory` and `MongoHistory` give identical `earlier_claims` for every claim of the 600 public claims (a Hypothesis-free exhaustive loop).
- [ ] **Step 2: Start Mongo** (`docker compose up -d mongo`, or the local one the previous session used), set `MONGO_URI`, run: RED. **Step 3: Implement. Step 4: GREEN with `REQUIRE_MONGO=1`.** Also run the access suite to confirm the shared compose edit broke nothing. Commit `feat(queue): MongoDB queue store passing the shared contract suite`.

---

### Task 6: Intake, outbox relay and reconciliation

**Files:** Create `src/workqueue/intake.py`, `src/workqueue/relay.py`, `src/workqueue/reconcile.py`, `tests/test_wq_intake.py`, `tests/test_wq_reconcile.py`.

**Interfaces:**
- Consumes: `triage.make_receipt`, `QueueStore`, `RoutingConfig` (latest from `store.latest_config()` or `DEFAULT`), `yara_engine.evaluate`, `SecurityLog.record`.
- Produces: `Intake(store, engine, securitylog, clock, rule_pack_hash, engine_version)`; `Intake.submit(claim) -> dict` (the receipt): validates the claim with the existing transport validation, runs the engine inline, builds the receipt with the configuration in force, calls `store.put_triaged` with `enqueue_pending: True`, and records the receipt in the security log (event `triage_receipt` added to `SECURITY_EVENTS` with `{'claim_id','input_hash','result_hash','lane','score','config_version'}`). A duplicate `(claim_id, input_hash)` returns the stored receipt and does nothing else. A changed claim body under an existing `claim_id` becomes `version + 1`.
- `relay.sweep(store, publish, now, max_age=60) -> dict` (`published`, `failed`, `stale`): for each pending outbox document call `publish(claim_id, version, input_hash)`; clear the marker only after `publish` returns without error; a publisher exception leaves the marker for the next sweep. `stale` counts markers older than `max_age`.
- `reconcile.reconcile(store, now, cfg_limits) -> Report` (`ok: bool`, `findings: list[dict]`): checks (1) every document is in a known state; (2) none sits in a state longer than its limit (`triaged` 60 s, `explained`/`explanation_skipped` 60 s, `leased` past `lease.expires_at`, `ready` older than 24 h is *reported* not repaired); (3) no outbox marker older than 60 s; (4) counts: documents created equals documents in some state, and every `decided` has `decided_by`; (5) no claim has two live leases. Stuck leased claims are repaired through the normal expiry path (`transition leased -> ready`, event `lease_expired`); everything else is only reported.

- [ ] **Step 1: Failing tests:** intake of a clean claim gives lane `green`, state `triaged`, `enqueue_pending True` and one security-log event; a duplicate submit returns the same receipt and the log has one event; a changed body is version 2 with a new receipt; a malformed claim is refused before any write; the receipt names the config version in force and a changed config changes it on the next claim; **Review Focus 5**: a `publish` that raises leaves `enqueue_pending True` and the next `sweep` publishes and clears it; a crash simulated between `put_triaged` and the first `sweep` (new `Intake`/`relay` objects on the same store) loses nothing; `sweep` run twice publishes once; reconcile on a fixture with one orphan per check reports exactly that orphan and repairs only the expired lease.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN on both stores** (parametrise over memory and Mongo when `MONGO_URI` is set). Commit `feat(queue): atomic intake, outbox relay and reconciliation`.

---

### Task 7: The dispatcher

**Files:** Create `src/workqueue/dispatcher.py`, `tests/oracle_dispatch.py`, `tests/test_wq_dispatcher.py`.

**Interfaces:**
- Consumes: `QueueStore`, `RoutingConfig`, permissions read through a callable `agents() -> list[Agent]` (`Agent(badge_id, permissions: frozenset, active: bool)`), supplied by the access service so permissions are always fresh.
- Produces: `dispatcher.plan_deal(ready, agents, inbox_loads, cfg, seed, now) -> list[tuple[str, str, int]]` returning `(claim_id, badge_id, version)` assignments, pure and deterministic. `dispatcher.Dispatcher(store, agents, clock, rng_seed=None)` with `deal(now=None) -> Deal` (applies the plan, logging the deal), `top_up_if_low(badge_id) -> int`, `next_for(badge_id) -> dict | None` (manual top-up of one claim), `expire(now=None) -> int`, `replay(deal_id) -> list` (re-derives the plan from the stored deal and returns it).

Algorithm for `plan_deal` (all of it, so the oracle and the code agree):
1. `cfg` is one version for the whole call (Review Focus 6).
2. Agents considered: `active`, in `cfg.on_shift`, with capacity `cfg.slice_size - len(inbox)` > 0; an agent needs a top-up only if its inbox holds fewer than `cfg.low_water` or this is a full `deal`.
3. Required permission for a claim: `claims.decide_high` when `receipt.eligibility == 'decide_high'`, else `claims.decide`.
4. Claim priority = `receipt.score + cfg.aging_per_hour * hours_since(state_at)`; order = shuffle with `random.Random(seed)` first (so ties are random but logged), then a **stable** sort by priority descending.
5. For each claim in order: candidates = agents who hold the permission, have capacity left, and are not excluded (`(badge, claim.patient_id)` in `cfg.exclusions`, or the badge decided an earlier version of this `claim_id`); pick the candidate with the lowest `load` (sum of scores already in the inbox plus dealt in this plan), ties broken by position in the seeded shuffle of agents; if none, the claim stays in the pool.

```python
def plan_deal(ready, agents, inbox_loads, cfg, seed, now):
    rng = random.Random(seed)
    order = list(ready); rng.shuffle(order)
    order.sort(key=lambda d: -(d['receipt']['score'] + cfg.aging_per_hour * max(0.0, (now - d['state_at']) / 3600)))
    pool = sorted(agents, key=lambda a: a.badge_id); rng.shuffle(pool)
    rank = {a.badge_id: i for i, a in enumerate(pool)}
    room = {a.badge_id: cfg.slice_size - inbox_loads[a.badge_id]['count'] for a in pool}
    load = {a.badge_id: inbox_loads[a.badge_id]['points'] for a in pool}
    out = []
    for d in order:
        need = 'claims.decide_high' if d['receipt']['eligibility'] == 'decide_high' else 'claims.decide'
        ok = [a for a in pool if need in a.permissions and room[a.badge_id] > 0 and not _conflict(a, d, cfg)]
        if not ok:
            continue
        pick = min(ok, key=lambda a: (load[a.badge_id], rank[a.badge_id]))
        out.append((d['claim_id'], pick.badge_id, d['version']))
        room[pick.badge_id] -= 1; load[pick.badge_id] += d['receipt']['score']
    return out
```

`Dispatcher.deal` loads `ready` documents and the inboxes, calls `plan_deal`, applies each assignment with `store.lease(...)` (a `None` means another dispatcher won; skip silently), writes the deal document `{deal_id, seed, ids_hash (SHA-256 of sorted eligible claim ids), config_version, agents: [{badge_id, permissions_sorted}], assignments, at}` with `store.append_deal`, and records one `claim_dealt` security event per assignment. `expire` moves expired leases back to `ready` with an event and a `lease_expired` security event.

- [ ] **Step 1: Write `tests/oracle_dispatch.py`**: an independent brute-force checker `check_plan(ready, agents, loads, cfg, plan)` asserting the invariants rather than re-deriving the order: each claim at most once; every assigned agent holds the needed permission, is on shift, within capacity; no excluded pair; **fairness**: after dealing, the spread of per-agent loads is no larger than the largest single claim score among dealt claims whenever all agents were eligible for all dealt claims.
- [ ] **Step 2: Failing tests:** worked example in code (3 agents, 9 claims, fixed seed, expected plan written out literally and also checked by the oracle); a high-eligibility claim is never given to an L2 without a per-user `claims.decide_high` grant, and is given to the L2 that has the grant; **Review Focus 4**: nobody on shift, only L2 on shift with only high claims, everyone at capacity: plan is empty and `deal` returns without error and the claims stay `ready`; aging: a score-2 claim waiting 40 hours (0.5/hour) outranks a fresh score-12 claim; conflict of interest: an agent who decided version 1 never receives version 2; an excluded `(badge, patient)` pair never receives that patient's claims; **reproducible deal**: run `deal`, then `replay(deal_id)` returns the identical assignments, and tampering with the stored seed changes them; **Review Focus 6**: a config change between loading and applying cannot change a running deal (the deal records and uses one `config_version`); **races**: 20 dispatchers calling `deal` in parallel threads over 200 ready claims on the memory store and on Mongo never put one claim in two inboxes and every claim is leased at most once; `top_up_if_low` does nothing at or above the low-water mark and fills to the slice size below it; `expire` returns claims to `ready` and a re-deal gives them to a different eligible agent when one exists; Hypothesis (150 examples) over random agents, claims, permissions: `check_plan` passes.
- [ ] **Step 3: RED. Step 4: Implement. Step 5: GREEN on both stores. Step 6: commit** `feat(queue): dispatcher with stratified balanced dealing, aging, conflict checks and replayable deals`.

---

### Task 8: AI explanation step, circuit breaker, budget and cache

**Files:** Create `src/workqueue/breaker.py`, `src/workqueue/explain.py`, `tests/test_wq_breaker.py`, `tests/test_wq_explain.py`.

**Interfaces:**
- `breaker.CircuitBreaker(clock, consecutive=5, error_rate=0.5, window=60.0, min_calls=20, cooldown=20.0)`; `.call(fn) -> result` raises `breaker.Open` when open, otherwise runs `fn`, records success or failure and re-raises the failure; states `closed -> open -> half_open -> closed`; **opens** on `consecutive` failures in a row or when failures / calls over the last `window` seconds is `>= error_rate` with at least `min_calls` calls; after `cooldown` seconds one probe is allowed (half-open), success closes, failure reopens. Only exceptions that subclass `breaker.Transient` count toward opening; `breaker.Fatal` (400 to 404) passes through without counting and is never retried.
- `explain.backoff(attempt, base=1.0, cap=30.0, rng) -> float` (full jitter: `rng.uniform(0, min(cap, base * 2**attempt))`).
- `explain.ExplainStep(store, model, guard, breaker, clock, rng, cfg_source, templates)`; `.run(claim_id, version) -> str` returns one of `'explained'`, `'explanation_skipped'`, moves the claim `triaged -> explained | explanation_skipped` with a state event naming the outcome (`ai_used`, `cache_hit`, `skipped_budget`, `skipped_rate`, `skipped_breaker`, `skipped_timeout`, `skipped_guard`, `skipped_error`) and, for skipped, stores the engine's deterministic explanation in `explanation`. Green claims are never sent here. Lane A and B only.
- Cache: template key = `sha256(rule_id | failure_shape | prompt_version | model)`; the stored text contains placeholders only (`{value}`, `{line}`), claim values are filled in afterwards, and the **existing clinical/fraud guard and the masking leak check run on the filled text**; only guard-approved text is cached. `store.bump('ai_minute', minute_key, cfg.ai_per_minute)` and `store.bump('ai_day', day_key, cfg.ai_daily_budget)` enforce the limits; a hard 90-second ceiling wraps the call (a deadline passed into the model adapter, not a thread kill).

- [ ] **Step 1: Failing tests (fake clock, fake model that fails, slows, recovers)** `test_wq_breaker.py`: opens after exactly 5 consecutive Transient failures and not after 4; a success in between resets the streak; opens at 50 % over the window only with at least 20 calls (19 calls at 100 % failure does not open through the rate rule, but 5 in a row still does); `Open` is raised without calling `fn`; after the cooldown exactly one probe passes, a second concurrent caller gets `Open`; probe success closes, probe failure reopens for a new cooldown; Fatal errors never count and never open; Hypothesis: the state is always one of the three and `call` never runs `fn` while open. `test_wq_explain.py`: success path moves to `explained` and caches the template; second identical failure shape is a cache hit with **no model call**; the cache key and cached text contain no claim value (inject a sentinel patient id and assert absent); filled text that fails the guard is not cached and the claim is `explanation_skipped` with the deterministic text; budget exhausted, rate exceeded, breaker open, model exceeding 90 s, model raising a fatal error: each gives `explanation_skipped` with the right outcome label and the claim reaches `ready` afterwards in the pipeline; a transient 503 is retried with jittered backoff (assert the sleeps with a fake sleeper) and at most the configured attempts; the step never raises into the worker for any model behaviour.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN, commit** `feat(queue): AI explanation step with circuit breaker, budget, template cache and guard`.

---

### Task 9: Celery adapter, Redis, CI and fault injection

**Files:** Create `src/workqueue/tasks.py`, `src/workqueue/pipeline.py`, `tests/test_wq_pipeline.py`, `tests/test_wq_tasks_eager.py`, `tests/test_wq_faults.py`. Modify `.github/workflows/ci.yml`, `docker-compose.yml`.

**Interfaces:**
- `pipeline.advance(store, claim_id, version, steps, now) -> str`: pure, idempotent: reads the state and does exactly the next step (`triaged`: green goes `ready`, lane A/B runs `ExplainStep` then `ready`); a call for a claim already past that state returns `'noop'`. Dealing is not a pipeline step (the dispatcher owns `ready`).
- `tasks.make_app(broker_url, backend=None) -> celery.Celery` configured with `task_acks_late=True`, `task_reject_on_worker_lost=True`, `broker_transport_options={'visibility_timeout': 3600}` (stated value, longer than the 90 s ceiling plus retries), `task_serializer='json'`, `accept_content=['json']`, `task_always_eager` only when `eager=True` is passed. Tasks: `process_claim(claim_id, version, input_hash)` (key `claim_id:input_hash` makes a duplicate a no-op), `deal()`, `expire()`, `relay_sweep()`, `reconcile()`. Beat schedule: deal every 15 s, expire every 60 s, relay every 10 s, reconcile every 5 min. A task that fails its retries writes a dead letter (`store.add_dead_letter`) and moves the claim to `dead_lettered`.

- [ ] **Step 1: Failing tests:** pipeline: green reaches `ready` with no model call; lane A with a working model reaches `ready` via `explained`; every claim of 100 generated fixtures reaches exactly one terminal-or-ready state (property); `advance` called twice is a no-op the second time; `test_wq_tasks_eager.py` runs `process_claim` with `task_always_eager`; **Review Focus 5 / fault injection (`test_wq_faults.py`)**: duplicate delivery of `process_claim` (call it three times) leaves one `explained` event; a worker that "dies" after the transition but before returning (raise after the write) and is redelivered does not repeat the model call; Redis lost (broker unreachable: `publish` raises) leaves markers pending and the relay republishes after "recovery"; AI timing out, over budget and failing the guard each still end `ready`; an exception inside the guard sends the claim to `dead_lettered` only after retries and creates one dead letter; an out-of-order task (`process_claim` for a claim already `leased`) is a no-op. Redis-backed tests (publish through a real broker, consume with one eager-off worker thread if possible, else only assert publish) are gated by `REQUIRE_REDIS`/`REDIS_URL` and skip loudly otherwise.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: CI:** add a `redis:7` service next to MongoDB in `ci.yml` (health cmd `redis-cli ping`), `REDIS_URL: redis://localhost:6379/0`, `REQUIRE_REDIS: '1'`. Update the compose header comment with the Redis line. **Step 5: GREEN locally** (Memurai is installed and Redis-compatible: `REDIS_URL=redis://127.0.0.1:6379/0`). Commit `feat(queue): Celery adapter, pipeline, Redis in CI and fault-injection suite`.

---

### Task 10: Queue service, API routes and decisions

**Files:** Create `src/workqueue/service.py`, `src/workqueue/api.py`, `tests/test_wq_service.py`, `tests/test_wq_api.py`. Modify `src/access/api.py` (one optional parameter and one `install` call), `src/access/service.py` only if a read of fresh permissions is missing (it should not be).

**Interfaces:**
- `QueueService(store, dispatcher, access_service, securitylog, review_log, clock)` with: `inbox(principal) -> list[view]`; `next(principal) -> view | None`; `heartbeat(principal) -> int`; `decide_finding(principal, claim_id, rule_id, action, reason) -> dict`; `decide_green(principal, claim_id, action)` (`action` in `verify_clear`, `escalate`); `dashboard(principal) -> dict`; `get_config(principal)`, `set_config(principal, changes) -> RoutingConfig`.
- Decision rules (all enforced here, not in the HTTP layer): the claim must be `leased` to `principal.badge`; permissions are **re-read from the store at this moment** through `access_service` (a token's cached permissions are not trusted); high-severity findings need `claims.decide_high`; the decision goes through the existing `review_workflow.validate_decision` and `review_log.append_review_decisions` with `actor` taken from the session; when every reviewable finding of the claim has a resolving decision (or the claim is green and `verify_clear`ed) the claim moves `leased -> decided` with `decided_by`; `escalate` on a green claim moves it `leased -> ready` with lane forced to the normal lane of its receipt, recorded; a lost lease raises `Conflict` (HTTP 409) and writes nothing.
- Routes (`api.install(app, queue_service, need, principal_dep, run)`; `create_app(..., queue=None)` calls it only when a queue service is given): `GET /api/work/inbox`, `POST /api/work/next`, `POST /api/work/heartbeat`, `POST /api/work/claims/{claim_id}/findings/{rule_id}/decision`, `POST /api/work/claims/{claim_id}/verify`, `GET /api/queue/dashboard` (needs `queue.view`), `GET /api/queue/config` and `PATCH /api/queue/config` (need `routing.manage`). Hide-not-disable applies: an inbox entry shows only the actions the caller may take, masked identifiers follow the existing masking, no route lets anyone assign or move a specific claim, and the admin cannot decide (level 4 lacks `claims.decide`).
- `set_config`: validates (Task 1), writes a new numbered version through `store.put_config(doc, expected_version)`, records `routing_config_changed` with before and after values; a stale `expected_version` returns 409.

- [ ] **Step 1: Failing tests:** an agent sees only their own inbox; they cannot decide a claim leased to someone else (403 or 404, never 200); **Review Focus 2**: demote the agent to L1 while a claim sits in their inbox, the next decision returns 403, nothing is written, and the next `expire`/`deal` returns the claim to the pool; **Review Focus 3**: the lease is expired and re-dealt while agent A is mid-request, A's decision gets 409 and B's succeeds, exactly one decision exists; the client cannot choose the actor, the original status or the state (mass assignment, extra body fields refused); a medium-only claim can be decided by an L2, a high one only by an L3 or a granted L2; an L4 admin gets 403 on every decision route and 200 on the dashboard and config; the admin cannot assign a claim (no such route; `POST` to a guessed URL is 404/405); a config change creates version N+1 with before/after in the security log and the next receipt names N+1; invalid config (negative points, slice below low-water) is refused whole with 422 and no version is written; the dashboard counts equal `store.counts()` and show the oldest-claim age and a shortage warning when a lane `decide_high` claim waits with no L3 on shift; green `verify_clear` records `claim_decided_green`; every route refuses an unauthenticated call (401) and a missing CSRF header exactly as the existing routes do (reuse the existing negative tests as a pattern).
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN** (memory store always, Mongo when configured), run the whole access suite. Commit `feat(queue): queue service and API with lease-bound decisions and capacity-only administration`.

---

### Task 11: Replay, rerun, dead letters and Mongo history for the extension rules

**Files:** Create `src/workqueue/replay.py`, `scripts/queue_admin.py`, `tests/test_wq_replay.py`.

**Interfaces:**
- `replay.replay_claim(store, engine, claim_id, version=None) -> dict` (`same: bool`, `old_hash`, `new_hash`, `diff: list`): re-runs the stored claim and compares `result_hash`.
- `replay.rerun(store, engine, old_rule_pack_hash, intake, dry_run=True) -> dict`: finds every claim whose receipt used `old_rule_pack_hash`, re-runs it, and (when not `dry_run`) re-triages **only claims whose results changed** as a new version with a new receipt; decisions on the old version stay in the log and are never rewritten.
- `queue_admin.py` subcommands `replay CLAIM_ID`, `rerun --rule-pack OLD [--apply]`, `deadletters`, `deadletter-replay ID`, `reconcile`, `verify-deal DEAL_ID`. It needs a level-4 operator: the script asks for badge, password and TOTP through the existing login service (use `getpass`), and records every use in the security log. Read-only subcommands still need `queue.view`.
- Extension findings: `intake` passes `MongoHistory(store)` (Task 5) into `extension_rules.evaluate_extensions`; the advisory E-results are stored in the document as `advisory` and shown in the reviewer inbox view, **never** in `results`, never in routing (`triage` is given only the official 15). A test pins that adding or removing advisory results leaves `lane`, `score` and `eligibility` unchanged.

- [ ] **Step 1: Failing tests:** `replay_claim` on an untouched claim is `same True`; after changing one rule's status in a fake engine it reports the diff; `rerun` dry-run changes nothing and lists the affected ids; `--apply` creates version 2 only for changed claims with a new receipt naming the new rule-pack hash; the version-1 decisions remain readable; `verify-deal` replays a stored deal and exits non-zero when the stored seed was altered; dead-letter replay moves the claim `dead_lettered -> triaged` once only; the script refuses to run without a valid operator and never prints a secret; advisory results do not affect routing; `MongoHistory` and `InMemoryHistory` give the same E101 to E103 statuses on the 600 public claims.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN, sink scan, bandit. Step 5: commit** `feat(queue): replay, rerun, dead-letter replay and database history for extension rules`.

---

### Task 12: Shadow mode, the experiment and mutation checks

**Files:** Create `src/workqueue/shadow.py`, `scripts/queue_experiments.py`, `tests/test_wq_shadow.py`, `tests/test_wq_experiments.py`, `tests/mutation_queue.py`. Evidence: `outputs/defense/queue.json` (force-added).

**Interfaces:**
- `shadow.predict_clear(receipt) -> float` baseline always `1.0` for green receipts and `0.0` otherwise (the pluggable interface the spec fixes; no trained model is built). `shadow.record(store, claim_id, version, prediction)` stores `{predicted_clear, at}` in `shadow`; it never changes state or routing. `shadow.agreement(store) -> dict` returns `n`, `agree`, and a Clopper-Pearson 95 % interval (implemented with the beta quantile by bisection on the exact binomial, no SciPy dependency), `None` bounds when `n == 0`.
- `queue_experiments.run(n_claims=500, n_agents=20, slice_size=25, seed) -> dict` using the public claims, a mix of L2/L3 agents in the proportion given by a CLI flag, an in-memory store, a fake clock and a simulated human who decides each claim after a fixed service time. Reports: slice size and overlap (**must be 0**), eligibility respected (**must be 100 %**), fairness (per-agent point totals: min, max, coefficient of variation), share of claims and queue depth per eligibility class, time to drain on the fake clock, lane counts, shadow agreement with its bound, cache hit rate, and the commit hash. The L3 bottleneck is reported honestly: with 4 L3 of 20, show the drain time and the effect of granting `claims.decide_high` to 4 more L2 agents.

- [ ] **Step 1: Failing tests:** shadow changes no state (compare the document before and after `record`); agreement with 0, 1, 50 and 100 decided claims: n=0 gives `None` bounds, n=50 with 50 agreeing has upper bound 1.0 and lower bound between 0.92 and 0.95, bounds are monotone in `agree`; `run` with a fixed seed twice gives identical JSON; overlap 0 and eligibility 100 % for 5 different seeds; an agent set with no L3 leaves high-severity claims undrained and the report says so with the count; evidence JSON validates against its own schema keys and names the commit.
- [ ] **Step 2: Mutation harness** `tests/mutation_queue.py` (same pattern as the earlier extension harness): mutants of `triage.lane` thresholds, `plan_deal` (drop the permission check, drop the conflict check, drop capacity, reverse the priority sort, ignore aging), `transition` (drop the `frm` guard), `lease` (drop the `ready` guard), `CircuitBreaker` (off-by-one on the streak, ignore min_calls, never half-open), `relay.sweep` (clear before publish), `decide_finding` (skip the permission re-read, skip the lease check). **Every mutant must be killed by some test; list survivors and add tests until none remain** (equivalent mutants documented by hand).
- [ ] **Step 3: RED, implement, GREEN. Step 4: run the experiment** `python scripts/queue_experiments.py --out outputs/defense/queue.json`, review the numbers, `git add -f outputs/defense/queue.json`. Commit `feat(queue): shadow mode, scale experiment and mutation suite`.

---

### Task 13: Two-person sign-off, documentation and final checks (cut the sign-off first if time is short)

**Files:** Create `docs/31_Task_Queue_and_Dispatcher.md`. Modify `src/workqueue/service.py`, `src/workqueue/states.py`, `tests/test_wq_signoff.py`, `README.md`, `SPECS.md` (new section 10f), `docs/superpowers/specs/2026-10-04-task-queue-dispatcher-design.md` (package name `workqueue`), the stale test counts everywhere (`Grep "1019"`).

- **Sign-off (isolated, optional):** add state `awaiting_countersign` (`leased -> awaiting_countersign -> decided | ready`). A high-severity finding confirmed or dismissed by one L3 leaves the claim `awaiting_countersign`, removes it from that L3's inbox and offers it only to a **different** L3 (`decided_by` of the first is excluded in `plan_deal`); the same action by the second resolves it; a different action escalates (claim returns to `ready` flagged `escalated` for a third L3) and is logged with both badges. Medium findings need one decision. Tests: the same badge cannot countersign (including after a re-deal), disagreement escalates, agreement decides, the first reviewer never sees it again, and with one L3 on shift the claim waits (and the dashboard says so).
- **Docs:** the card document explains the pipeline with a Mermaid diagram, the routing formula and a worked example, the dealing algorithm, the outbox, the breaker, the caching table with what is never cached, replay and reconcile, how to run the stack (`docker compose up -d mongo redis`; the Windows demo runs Celery eager, Linux workers run in Docker) and the open risks from the spec. README gets a row; SPECS 10f gets the long section the user asks for on every sub-project, citing `outputs/defense/queue.json`.
- **Final checks (all must be run and shown, not assumed):** whole suite on 3.10, 3.12 and 3.14 with `REQUIRE_MONGO=1 REQUIRE_REDIS=1`; `bandit -q -r src scripts -ll` exit 0; `pip-audit` clean for the two new pins; the byte-pin tests for the official engine files still pass; loop the new Hypothesis modules 20 times each for flakes; one fresh reviewer (model `fable`) over the whole branch, one fix pass; then ask the user before pushing.
- [ ] **Step 1: Failing sign-off tests, RED. Step 2: Implement. Step 3: GREEN. Step 4: write docs. Step 5: run every final check and paste the outputs into the commit message body. Step 6: commit** `feat(queue): two-person sign-off for high severity, card document, README and SPECS 10f`.

---

## Self-review

- **Spec coverage:** pipeline and AI-never-blocks (Tasks 8, 9); triage formula, receipt, oracle (2); eligibility and the L3 pressure point (2, 7, 12 report); state machine and events (3, 4); reconciliation and outbox (6); components and the truth/broker split (4, 5, 9); personal topped-up inboxes, leases, stratified dealing, aging, conflict of interest, reproducible deals, expiry (7); two-person sign-off (13); green `verify_clear` (10); administration, capacity only, versioned config, dashboard (1, 10); AI step with breaker, budgets, 90 s ceiling, guard (8); caching and never-cache rule (8, 10 re-read); replay, rerun, dead letters, shadow mode, hash chain (6, 11, 12); testing list incl. races, faults, mutation, experiment (4 to 12); mentor question on a trained shadow model stays a plain interface (12).
- **Deliberate departures from the spec text:** package renamed `workqueue` (stdlib clash); short-lived counters live in the store (`bump`), not Redis, so budgets survive a Redis loss and the twin can be tested without Redis. Redis stays broker only.
- **Open items I did not invent answers for:** whether a trained model may sit in the decision path (mentor question), exact AI explanation prompt wording (reuse the existing prompt v1.6.0 and guard), production deployment of Celery workers (Docker, out of scope per spec).
