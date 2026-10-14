# Human-in-the-loop confidence routing and the learning loop: design

Date: 2026-10-14. Branch: `rust-rewrite`. Supersedes the Python-era spec `2026-10-07-active-learning-design.md` (its statistics, configuration and failure rules are kept; its placement moves to Rust). Covers two rubric items with one mechanism:

- **Human-in-the-Loop and Escalation Logic (10 points):** reviewer-facing routing that flags low-confidence or high-severity cases for explicit human approval, with manual overrides and feedback logging.
- **Active Learning Feedback Loop from Human Overrides (2 points).**

## 1. Why one design

Nearly all of the 10-point item is built (lanes, personal inboxes, lease-bound decisions, two-person sign-off, dismiss-with-reason overrides, the feedback report). The one missing clause is **low confidence**. The rule engine is deterministic and its `confidence` is `null` by design (docs/04), and the AI never judges, so there is no honest confidence number to route on. Reviewers do produce one: for every finding they confirm it or override it. The share of a rule's findings that people uphold, with an exact interval, is a **measured reliability**. It is the confidence signal for routing, and it is also the thing the learning loop learns. The two items are two halves of one loop: reviewers decide, the system measures, routing changes, reviewers decide again.

## 2. Success criteria (each one is tested)

1. A claim is flagged for **explicit approval** when any flagged rule is high severity (existing), **or** its rule is measured and **doubtful** (new low-confidence reason), **or** the result is `UNABLE_TO_ASSESS` or degraded (existing). Each inbox item carries the list of reasons, as words and no numbers.
2. A doubtful finding goes to a senior (eligibility `decide_high`). A medium-severity doubtful finding needs one senior; the two-person sign-off stays reserved for high severity.
3. A reviewer can override: dismiss a finding with a reason, escalate, or request information (all existing). Every override is a counted feedback record.
4. A rule that reviewers keep overriding becomes doubtful after enough independent decisions; one reviewer cannot move the estimate alone.
5. No verdict, status, rule, evidence or AI output changes. Results are byte-identical with the feature on or off.
6. Every estimate is versioned, every receipt names the version it used, and an estimate can be re-derived and checked.
7. Defaults are off: with the feature off every existing test, receipt and evidence file is unchanged.

**Not in scope.** A trained language or neural model. Showing reliability numbers to reviewers (anchoring would corrupt the labels). Changing the 15 rules or the triage formula. Real-reviewer accuracy: all evidence uses simulated reviewers and says so.

## 3. Approaches considered

| Approach | Verdict |
|---|---|
| **A. Measured per-rule reliability from overrides, thresholded for routing, uncertainty-first ordering** | **Chosen.** Counts and exact intervals. Deterministic, explainable, replayable. Needs no trained model, so it needs nothing from the mentor |
| B. A learned routing policy (logistic regression on logged deals with inverse-propensity weighting) | Stage 2, offline experiment only, never in the decision path. The dispatcher deals at random so deal probabilities are logged; this is the principled way to learn a policy from them. Run it in the lab to show the path, promote it only after real data and a mentor answer |
| C. A model predicting "a human will uphold this finding" | Already covered by shadow mode (records, acts on nothing) |
| D. Ask the language model how sure it is | Rejected: poorly calibrated, and the AI must never influence routing |

Naming: this is **learning from human feedback for routing**. It is not reinforcement learning from human feedback on a language model, and the documents say so.

## 4. Definitions

**Counted decision.** For each decided dossier and each flagged rule, the last resolving action (`confirm_issue` or `dismiss_with_reason`) across rounds, with its actor and time. `confirm_issue` is *upheld*; `dismiss_with_reason` is *overridden*. Information requests, escalations and recheck marks are not counted.

**Reviewer cap.** For a rule with `N0` counted decisions, one reviewer is limited to `max(1, floor(reviewer_cap * N0))` of them (their earliest by time, claim id, version).

**Per-rule statistics** over the capped decisions: `upheld` k, `n`, distinct `reviewers`, `rate = k/n`, exact 95 % Clopper-Pearson `[lower, upper]` (the routine already in `cg-queue/src/shadow.rs`), `width = upper - lower` (1 when `n = 0`).

**Status.** `unmeasured`: `n < reliability_min_decisions` or `reviewers < reliability_min_reviewers`. `doubtful`: measured and `upper < doubtful_below`. `measured`: otherwise. Using the upper bound means a small sample never causes doubt, and an unmeasured rule routes exactly as today (no false alarm at cold start).

**Claim uncertainty** = the largest `width` among its flagged rules (0 when none).

## 5. Behaviour

**Learning.** `learn(store, cfg, now)` reads decided dossiers, computes the statistics and returns the next snapshot; if counts or any status changed and `learn_enabled`, it is stored as version N+1 and `reliability_updated` is written to the security log. A snapshot holds version, time, parameters, per-rule statistics, the number of decisions and `inputs_hash`; no claim id, badge or reason. `verify` recomputes every figure from the stored counts. A scheduled task runs it every 600 s; `cg queue-admin learn [--apply]` runs it by hand.

**Routing.** When `route_by_reliability` is on, intake applies the latest snapshot after the normal receipt: a doubtful flagged rule sets eligibility `decide_high`; the receipt gains `reliability_version`, `doubtful_rules`, `uncertainty` and `attention` (the reason list from criterion 1). Lane and score do not change (the published formula is untouched). Routing is fixed at intake; a later snapshot changes later claims only.

**Active learning.** Dispatcher priority becomes `score + aging_per_hour * hours_waited + uncertainty_weight * uncertainty`; weight 0 changes nothing. The deal snapshot stores the uncertainty so replay and verify stay exact.

**Visibility.** `GET /api/v1/queue/reliability` (`queue.view`) and the operator tool show the snapshot; reviewers see only the plain-word reason in `attention`.

## 6. Configuration

Seven new `RoutingConfig` fields, versioned and validated like the others, older documents load with the defaults:

| Field | Default | Range |
|---|---|---|
| `learn_enabled` | false | boolean |
| `route_by_reliability` | false | boolean |
| `reliability_min_decisions` | 20 | 5 to 1,000 |
| `reliability_min_reviewers` | 3 | 1 to 50 |
| `reliability_reviewer_cap` | 0.4 | 0.1 to 1.0 |
| `doubtful_below` | 0.6 | 0.05 to 0.95 |
| `uncertainty_weight` | 0.0 | 0 to 20 |

## 7. Components (Rust, `cg-queue` unless noted)

| Unit | Responsibility |
|---|---|
| `reliability.rs` (new) | `compute`, `learn`, `apply`, `verify`; pure except `learn`; no import of the API, dispatcher or AI step |
| `routing_config.rs` | the seven fields |
| `store.rs`, `cg-mongo/queue.rs` | `put_reliability(doc, expected_version)`, `latest_reliability`, `reliability_history`; collection `reliability`, unique `version`; one contract suite on the memory twin and on MongoDB |
| intake / `triage.rs` | applies the snapshot when routing is on; `make_receipt` stays unchanged, the extras are added after it |
| `dispatcher.rs` | uncertainty term in priority and in the deal snapshot |
| `cg-api` | `GET /queue/reliability`; the config body accepts the new fields; `attention` in the inbox view |
| `cg-cli` | `queue-admin learn`, `reliability`, `verify-reliability` |

The simulation lab is Python and lives in `scripts/sim/`: it is offline tooling that writes files, not part of the server. A Python reference of the statistics is the differential oracle for the Rust code, as elsewhere in this project.

## 8. Failure behaviour

| Situation | Behaviour |
|---|---|
| No snapshot, or the store call fails | Routing proceeds as if the feature were off; nothing blocks |
| A snapshot fails `verify` | Ignored for routing, a security event is written, the API and tool mark it altered |
| Too few decisions or reviewers | `unmeasured`, never doubtful |
| One reviewer dismisses everything | The cap and the minimum-reviewers rule stop it |
| `learn` twice on the same data | The second run writes nothing |
| Two `learn` runs race | The unique `version` index lets one win |
| Config changes mid-run | Each receipt and snapshot records the version it used |

## 9. The simulated review database

Real reviewer data does not exist yet, so a seeded generator (`scripts/sim/make_reviews.py`) builds it, so the whole loop can be developed and tested now. Everything it writes carries `simulated: true`.

- **Findings are real.** Claims come from `data/development/claims.jsonl` and their findings from the engine results (`expected_results.jsonl`); only the humans are simulated.
- **Reviewers:** about 12 across L2 and L3, in personas: careful, lenient, noisy, slow, and one adversarial reviewer who dismisses nearly everything.
- **Hidden truth:** each rule has a hidden true upheld rate (most about 0.97, R006 0.35, R013 0.5). A reviewer's decision is the truth plus persona noise, so the learner has something to recover and the experiments can check it did.
- **Output:** JSONL in the shape the queue already stores (dossier, decisions with actor and time, deal snapshots with seed), loadable into the memory store and into MongoDB; about 5,000 decisions, size adjustable.

State plainly in every document: the reviewers are simulated, so this shows the mechanism and not real-world accuracy.

## 10. Evidence (`scripts/learning_experiments.py` to `outputs/defense/learning.json`, added with `git add -f`)

1. **Convergence:** estimate, interval and status over time, and the number of decisions until R006 becomes doubtful.
2. **Routing effect and senior load:** share of claims sent to seniors with routing off and on, and the resulting senior queue and waiting time under the same arrivals. docs/31 shows 253 of 500 claims already need a senior, so doubtful-to-senior could worsen it; if it does, the document says so and names the setting (`doubtful_below`, or leaving routing off) that limits it. Hash comparison proves no verdict changed.
3. **Active-learning gain:** with a fixed review budget, uncertainty-first against arrival order. Worded as a demonstration of the mechanism, not a finding: ordering by interval width shrinks intervals faster almost by construction.
4. **Poisoning:** one adversarial reviewer among five, with and without the cap and the minimum-reviewers rule.
5. **Label shift:** once a rule is doubtful its later labels come mostly from seniors, who may uphold at a different rate; the estimate then mixes two populations. Reported with the simulation and listed as a limit: the estimate describes the people who reviewed, not the rule in the abstract.
6. **Stage 2 (offline only):** a logistic-regression routing policy fitted on logged deals with inverse-propensity weighting, evaluated on a split it never saw, compared with the threshold rule. Reported as exploratory.
7. **Invariants:** receipts with the feature off equal the baseline on the 600 public claims; a tampered snapshot is detected; replay of a deal with an uncertainty term still matches.

## 11. Testing

Unit tests against known intervals; the cap; status transitions; determinism under any order of decisions. Property tests (`lower <= rate <= upper`; counted never exceeds total; snapshot identical for any permutation). Rust and the Python reference agree on a few thousand random count pairs. Store contract on the memory twin and MongoDB. Intake with the feature off, on, with no snapshot, and with an altered snapshot. Dispatcher priority and replay. API permission matrix and the `attention` field. Mutation tests on the new code. The full workspace test run with MongoDB and Redis required, and existing evidence files unchanged.

## 12. Documentation

`docs/47_HITL_Confidence_and_Learning_Loop.md`; SPECS.md section 10h; README (the Phase 2 rows for both rubric items, which now say "Not built: routing by model confidence", plus the briefing); `SPECIFICATION.md` (FR-22, a new requirement, a "Learn from overrides" use case, data model).

## 13. Open question for the mentor (does not block this work)

May a *trained* model ever influence routing (who reviews and in what order), never the verdict? This design uses none. If yes, Stage 2 becomes a follow-up trained on the decisions this loop collects.
