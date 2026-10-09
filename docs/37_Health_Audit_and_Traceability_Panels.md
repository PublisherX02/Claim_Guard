# System health, audit and claim trace (administrator panels)

Three pages in the console, for administrators only. Nothing here decides anything about a claim. They answer three questions:
**is anything broken, what happened and who did it, and can every step of one claim be followed from intake to decision?**

| Page | Address | Who sees it (permission) | Reads |
|---|---|---|---|
| System health | `#/health` | level 4, `queue.view` | `GET /api/v1/ops/health` |
| Audit and traceability | `#/audit` | level 4, `audit.view` | `GET /api/v1/ops/audit/summary`, `GET /api/v1/audit/events` (filters), `GET /api/v1/audit/verify` |
| Claim trace | `#/trace/<claim id>` | level 4, `audit.view` | `GET /api/v1/ops/trace/<claim id>` |

Everyone else does not see these entries in the menu (hide, not disable), and the server refuses the calls anyway: a refusal is itself written
to the security log (`forbidden`). `tests/test_ops.py` checks this for every level and for a caller who is not signed in.

## 1. System health: how it decides "something is broken"

Each check is a small probe that reports **ok**, **degraded**, **down** or **unknown** (it has not reported yet), a one-line reason, how long it took
and when it ran. A probe that crashes is reported as **down**, never as an error page, so a broken check cannot hide the problem it was meant to find.

The headline is worst-of over the **core** services (the system cannot work without them); the **optional** helpers can only make it *degraded*.

| Check | Weight | Down / degraded when |
|---|---|---|
| Reviewer API | core | more than 10% of the last 15 minutes' requests (at least 20) failed with a server error = down; any server error = degraded |
| User database | core | does not answer a ping = down |
| Audit logs (hash chains) | core | either log fails verification = **down** ("treat the history as untrusted until explained"). Verified at most every 5 minutes, and whenever an administrator presses Verify |
| Disk space for the logs | core | under 10% free = degraded, under 2% = down (a full disk stops the audit logs from being written) |
| Log anchor | core | no anchor key configured, or the anchor is not signed = degraded |
| Queue database | core | does not answer a ping = down |
| Message broker (Redis) | core | does not answer within 1 s = down; in demo mode it reports the in-process stand-in |
| Scheduled jobs and workers | core | the four jobs (relay every 10 s, deal 15 s, expire 60 s, reconcile 300 s) each leave a heartbeat in the queue database. Last run older than 3 times its interval = **down**; last run failed = degraded; never ran = unknown |
| Publishing of new claims | core | claims waiting in the outbox for more than 2 minutes = degraded (the relay or the broker is stuck) |
| Claim flow | core | dead letters, lapsed leases not returned within 3 minutes, a claim waiting more than 30 minutes, or no one on shift who may decide waiting claims = degraded |
| AI explanation helper | optional | circuit breaker open or half-open = degraded. Claims are never blocked by it: explanations fall back to the engine's own text |
| External timestamp (RFC 3161) | optional | none = unknown; stamped but stale = still ok; invalid = degraded |
| Security signals | optional | an account lockout in the last hour, or 20 or more failed sign-ins = degraded |

Beside the checks the page shows **traffic to this API**: requests per minute for the last hour (succeeded, refused or invalid, server errors),
the response-time distribution (an upper bound per bucket, not an interpolation, so it never flatters), the busiest routes and the last server
errors. These come from a small in-process meter (`src/access/ops.py`) that counts method, route shape and status only: identifiers are collapsed
(`/claims/:id/...`), query strings and bodies are never recorded, the route table is capped, and the error list keeps the last 50.

**Limits, stated on the page and here:** the traffic numbers describe *this API process since it started*, not a fleet. The scheduled-job and queue
checks do read the shared database, so they are fleet-wide. The page refreshes every 15 seconds and can be refreshed on demand.

## 2. Audit and traceability

* **Counts and chart:** events on record, failed sign-ins, lockouts, refused requests, rejected sessions; events per hour for the last 24 hours with
  failures shown separately; what is logged most; who was most active.
* **Filters:** event type, badge, claim id, period (1 hour, 24 hours, 7 days, all time), newest first, paged. Filters are exact matches; an unknown
  event type or a malformed time is refused with a 422.
* **Plain-language lines:** every event type is rendered as a sentence ("CG-2002 confirm issue on R013 of CG-...") instead of raw JSON.
* **Download CSV:** the current filter, at most 1,000 rows, with cells that start with `=`, `+`, `-` or `@` neutralised so a spreadsheet cannot run them.
* **Verify the logs:** re-checks both hash chains and the anchor on demand and records who asked and the result.
* **Reading the log is itself logged** (`audit_read`), so an administrator's looking is also traceable.

## 3. Claim trace

For one claim id: every stored **version** (a corrected resubmission is a new version), and for each version
* the triage receipt: lane, score, eligibility, settings (routing configuration) version, rule-pack hash, engine version, and the hashes of the input and the results,
  so any later claim of "this is what the system saw" can be checked against the stored bytes;
* the full state history (received, triaged, explained, ready, leased, decided, ...) with who or which worker caused each change and when;
* every decision: when, who, which rule, which action, the written reason;
* which findings were flagged;
* below the versions, every security-log entry that names the claim (dealt to whom, triaged, signed off, unmasked).

A trace contains **no claim values** (no patient, member, invoice, amounts): it says who did what and when, and the hashes tie each step to the exact data.
It can be downloaded as JSON for an auditor. Tests assert that no identifier from the claim appears in the answer.

## 4. What was added underneath

| Piece | Where |
|---|---|
| Request meter and the health report builder | `src/access/ops.py` |
| Queue probes (store, broker, jobs, outbox, flow, AI breaker) | `src/workqueue/health.py` |
| Job heartbeats (`record_job`, `jobs`, `ping` on both queue stores, same contract tests) | `src/workqueue/jobs.py`, `store.py`, `store_mongo.py` |
| Filtered, single-pass audit scan | `SecurityLog.scan` in `src/access/securitylog.py` |
| Claim trace | `QueueService.trace` in `src/workqueue/service.py` |
| Charts (inline SVG, no library, no markup sinks) | `src/access/ui/lib.js`, `views-ops.js` |
| Tests (unit, permission matrix, both stores) | `tests/test_ops.py`, `tests/queue_store_contract.py` |

## 5. Honest limits

* No alert is *sent* anywhere (no email or chat): the panel shows problems to whoever looks. Wiring the same report to a notifier is a small step because it is one JSON endpoint.
* The traffic meter is per process and in memory; it resets when the API restarts.
* The audit chain proves the log was not edited after the fact; it does not prove the events were true when written.
* The health page runs with the API's own credentials; if the API itself is down the page is down. An outside check on `/healthz` remains the last line (the Docker stack already uses it).
