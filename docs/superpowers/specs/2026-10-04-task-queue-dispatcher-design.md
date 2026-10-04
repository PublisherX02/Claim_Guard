# Phase 2, sub-project 3: triage, task queue and work dispatcher

Serves the Phase 2 rubric line "Human-in-the-loop and escalation logic" (10 points: routing of low-confidence and
high-severity cases, overrides, feedback log) and, through the replay and reconciliation tooling, the traceability half of
"Privacy, security and safety guards". Stacks on the identity-and-access work (open PR from `phase1-hardening-and-defense`);
this branch starts from commit `a1fd054` and must be rebased if that PR changes before it merges.

## Goal
Every claim that arrives is checked by the rules engine, given a **triage receipt**, placed in a **lane** by a published
formula, explained by the AI where there is something to explain, and handed to a human by a **dispatcher** that gives each
agent a random slice of the work they are allowed to do. Every step is recorded, replayable and reconcilable, so a mistake
can be found and redone cheaply. **No claim is ever cleared without a human in this version.**

## Non-goals
Auto-clearing any claim (recorded only as *shadow mode*, below); a web front end; real recheck of corrected claims through the
API beyond re-entering triage; multi-node MongoDB or Redis clustering; Kubernetes manifests; the market-rules extension work.

## The pipeline
```
claim ─► rules engine (inline, milliseconds) ─► triage receipt + state "triaged" + outbox marker   (one atomic write)
                                                   │ relay publishes the marker to Celery, then clears it
        ┌──────────────────────────────────────────┴─────────────────────────────┐
        ▼ green (nothing flagged)                  ▼ lane A (1 to 3 flagged)     ▼ lane B (4+ flagged or score ≥ 10)
   no AI call                                 AI explanation task            AI explanation task
        └────────────────────► dispatch pool (state "ready") ◄──────────────────┘
agent asks for work ─► dispatcher draws a random slice of what that agent may decide ─► lease ─► decision ─► state "decided"
```
The AI never decides anything and never blocks the queue: if it is slow, over budget, down, or fails the output guard, the
claim becomes `ready` with the engine's own deterministic explanation.

## Triage and the routing formula
A *flagged* finding is a result whose status is `FAIL` or `UNABLE_TO_ASSESS`. Points per flagged finding:

| Status | high severity | medium severity |
|---|---|---|
| FAIL | 4 | 2 |
| UNABLE_TO_ASSESS | 2 | 1 |

`score` = sum of points. Lanes: **green** = nothing flagged; **A** = 1 to 3 flagged and score < 10; **B** = 4 or more flagged,
or score ≥ 10. The two numbers (3 flagged, score 10) are defaults held in the routing configuration and tunable by an administrator.
The formula is checked by `tests/oracle_routing.py`, an independent implementation, on Hypothesis-generated result sets (every status and
severity combination across the 15 rules) plus the full evaluation sets.

**Who may take a claim (eligibility)** is separate from the lane and follows the permissions already built:
green → any level with `claims.decide` (L2, L3); any flagged high-severity finding → `claims.decide_high` (L3); otherwise L2 or L3.
**Known pressure point:** 11 of the 15 rules are high severity, so almost every non-green claim needs an L3. The experiment
reports the share of claims per eligibility class and the queue depth per class under the 500-claim/20-agent scenario; the
mitigation, per-user `claims.decide_high` grants for experienced L2 reviewers, already exists. Splitting one claim across agents by
finding is out of scope.

## Triage receipt (written before anything is queued, green claims included)
`claim_id`, `input_hash`, `rule_pack_hash`, `engine_version`, `facts_hash`, `result_hash`, per-finding statuses, `score`, `lane`,
`eligibility`, `config_version` (the routing configuration in force), `created_at`. It is also appended to the hash-chained audit
log, so "why was this claim green?" always has an answer that cannot be edited afterwards.

## State machine (stored on the claim, every move an append-only event)
`received → triaged → explained (or explanation_skipped) → ready → leased → decided → (rechecked → triaged)`; side states
`dead_lettered`. Each transition records the worker or badge, timestamp, and the previous state; a transition from the wrong state
is refused. A **reconciliation job** checks that every received claim is in progress or finished, that nothing sits in a state
longer than its limit, that no outbox marker is older than a minute, and that counts in equal counts out; any orphan is reported
and, for stuck `ready`/`leased` claims, repaired by the normal expiry path.

## Crash safety: the outbox
At intake one atomic single-document write stores the claim, its receipt, state `triaged` and `enqueue_pending: true`. A relay
publishes pending claims to Celery and clears the marker. A crash between the write and the publish loses nothing: the relay
republishes on its next sweep. Everything lives in one document because multi-document transactions need a replica set and the
compose file runs one node. Tasks are keyed by `claim_id + input_hash`; a duplicate delivery is a no-op.

## Components and what holds the truth
- **MongoDB is the source of truth** (claims, work state, leases, draws, routing configuration and its history, dead letters,
  explanation cache). **Redis is only the Celery broker and short-lived counters.** Losing Redis loses nothing that the relay and
  reconciliation cannot republish.
- **A pure Python core** (`src/queue/`: `triage`, `states`, `dispatcher`, `explain`, `replay`, `reconcile`) with a store interface
  implemented in memory and on MongoDB, tested by one contract suite on both (the pattern of `access_store_contract.py`). **Celery is a thin adapter** (`src/queue/tasks.py`):
  tests run it with `task_always_eager`; real runs use Linux workers in Docker because Celery's default pool does not run on Windows.
  `acks_late=True`, `task_reject_on_worker_lost=True`, and the Redis visibility timeout pinned to a stated value; idempotency
  absorbs the redeliveries that setting allows.
- **CI** gains a Redis service next to MongoDB and a `REQUIRE_REDIS` gate (the Redis-backed tests fail rather than skip).

## The dispatcher
- **Each agent has a personal inbox that the dispatcher keeps filled.** No agent waits to ask. A dispatch task deals claims from
  the `ready` pool into the inbox of every agent on shift until each holds the slice size (default 25); when an inbox falls below
  a low-water mark (default 10) it is topped up at once. Claims move pool to inbox with a conditional `find_one_and_update`, so two
  agents can never hold the same claim, even in parallel. Each inbox entry is a **lease** that expires if the agent goes silent
  (default 30 minutes without a heartbeat or decision); expired entries return to the pool and are dealt to someone else. The agent's
  screen (`GET /work/inbox`) simply lists their inbox; `POST /work/next` remains as a manual top-up.
- **Dealing is stratified-random, so workloads come out even.** Eligible claims are shuffled with a logged seed, then dealt like
  cards in priority order across the agents who may take them, so every inbox receives a similar total of score points as well as
  a similar count. Randomness decides which claims, never how much work. Unevenness is reported by the experiment.
- **Aging.** Priority is the score plus a bonus per hour waited, so the hard lane cannot starve behind new arrivals; the random draw
  is replaced by dealing in priority order.
- **Conflict of interest.** An agent never receives a claim whose earlier version they decided, or for the same patient pseudonym
  where the administrator has marked a relationship (kept as an exclusion list in the routing configuration).
- **Reproducible deals.** Each deal logs the seed, the SHA-256 of the sorted eligible claim ids, the badges and eligibility of the
  agents on shift at that moment, and the resulting assignments. A test replays a logged deal and must obtain identical inboxes.
- **Expiry and reassignment.** An expired lease returns its undecided claims to `ready`; each reassignment is logged.
- **Two-person sign-off for high severity** (last task, isolated, cut first if time is short): a high-severity finding confirmed or
  dismissed by one L3 becomes `awaiting_countersign` and resolves only when a *different* L3 agrees; a disagreement escalates
  and is logged. Medium findings need one decision.
- **Green claims** receive a claim-level action `verify_clear` (or `escalate`, which sends the claim to its normal lane) recorded
  as an audit event bound to the badge, like finding decisions.

## Administration (capacity, never individual claims)
New L4-only permissions `routing.manage` and `queue.view`. The administrator can change: which badges are on shift, slice size,
lease length, the two lane thresholds, the points table, aging rate, AI budget and rate limit, and the conflict exclusion list.
The administrator **cannot assign or move a specific claim** and still cannot decide claims. Every change creates a new numbered
configuration version, written to the security audit log with before and after values; each receipt names the version it used.
The dashboard (`queue.view`) shows depth per lane and eligibility class, age of the oldest claim, active leases, dead letters and
who is on shift, so a shortage of L3 agents is visible before it hurts.

## AI explanation step
A Celery task per flagged claim. It runs only for lanes A and B. Limits: a per-minute rate and a daily budget (administrator
setting); a **circuit breaker** around the model call (opens after 5 consecutive failures, or an error rate of 50% over a 60-second
window of at least 20 calls; retries only transient errors such as 429 and 503, with exponential backoff and jitter; 15 to 30
seconds of cooldown, then a half-open probe; fatal errors such as 400 to 404 are never retried) and a hard 90-second ceiling per
explanation; beyond any of these the claim is `explanation_skipped` and dispatched with the deterministic text. Output goes through the
existing clinical and fraud guard; on failure the deterministic text is used and the failure is logged. Each outcome (AI used,
cache hit, skipped, guard-rejected) is a state event.

## Caching (and what is never cached)
| Cached | Key | Why it is safe |
|---|---|---|
| Rule results | `input_hash + rule_pack_hash` | The engine is deterministic; a rule change changes the key. Also the basis of replay checks |
| AI explanation template | rule, failure shape, prompt version, model | The key and stored text contain no identifiers; claim values are filled in afterwards and **the guard and the masking leak check run on the filled text** |
| Reference data | file hash | Loaded once per worker |
| Dashboard counts | none (short TTL) | A few seconds of staleness is acceptable |

**Never cached: permissions, sessions or clearance.** They are read from the database on every request so a demotion takes
effect at once (a mutation test already pins this). Only guard-approved explanations enter the cache; a prompt, model or rule
change invalidates entries.

## Traceability, replay and redo
1. Receipt for every claim (above).
2. **Replay**: `scripts/queue_admin.py replay CLAIM_ID` re-runs a claim and compares result hashes. After a rule fix,
   `rerun --rule-pack OLD` finds every claim processed under the old pack, re-runs them, prints the diff and re-triages only the
   claims whose result changed (a new version and a new receipt; decisions on the old version stay in the log).
3. State events and reconciliation (above).
4. **Dead-letter queue**: a task that fails its retries is stored with the reason and visible to the administrator, who can replay it.
5. **Lease trail** and reproducible draws (above).
6. **Shadow mode**: the system computes "this green claim would be auto-cleared" and stores it; the claim still goes to a human.
   The experiment reports agreement between that prediction and the human decision with a Clopper-Pearson bound, which is the
   evidence needed before auto-clearing is ever switched on. It has no effect on any claim.
7. Hash-chained log and anchor cover receipts, configuration changes and draws.

## Testing
- Contract suite on the in-memory and real MongoDB stores; Redis/Celery tests gated by `REQUIRE_REDIS`; the circuit breaker is tested with a fake clock and a fake model that fails, slows and recovers.
- Routing formula against the independent oracle; property tests: every claim reaches exactly one terminal state; no claim is in
  two leases; a leased claim is always eligible for its holder; expired leases return claims; replaying a logged draw reproduces it.
- Fault injection: crash between the atomic write and the publish; worker killed after acking; duplicate delivery; Redis lost;
  AI timing out, over budget, and failing the guard.
- Races: parallel draws by many agents never overlap; a demotion during a lease is honoured on the next request.
- Mutation checks of the new tests, as before.
- Experiment `scripts/queue_experiments.py`: 500 claims from the public sets, 20 agents, slices of 25. Reports: slice size and
  overlap (must be 0), eligibility respected, fairness across agents, share per eligibility class, time to drain, lane counts,
  shadow-mode agreement, and cache hit rate. Evidence written to `outputs/defense/queue.json` with the commit that produced it.

## Differences from the first sketch (recorded decisions)
Lanes come from a severity-weighted score, not a bare count; green claims are verified by a human; the AI never blocks;
work is pushed into personal inboxes that stay topped up (no waiting), with expiring leases so an absent agent never strands claims; the administrator sets capacity, never individual assignments; the engine runs
inline at intake; and a cache exists only where it is provably safe.

## Shadow-mode model (undecided; mentor question pending)
The shadow predictor stays a plain interface (`predict_clear(receipt) -> probability`) with a trivial baseline (always "clear")
until enough human decisions exist. The candidate models go to the mentor; none is built in this sub-project. Whatever is chosen is
trained only on human decisions already in the log, evaluated with a bound, and never able to change a claim's route.

## Open risks to report honestly
High-severity concentration on L3 (above); Celery needs Linux workers (Docker) so the Windows demo runs eager; dealing by priority with
aging trades strict first-come order for no starvation; shadow-mode agreement can only be measured once humans have decided enough claims.
