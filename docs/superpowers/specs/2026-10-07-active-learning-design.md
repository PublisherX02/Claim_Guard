# Active learning feedback loop from human overrides: design

Date: 2026-10-07. Branch: `phase2-queue-dispatcher`. Sub-project 4 of Phase 2. Bonus item "Active Learning Feedback Loop from Human Overrides" (2 points), and the missing half of the human-in-the-loop line "routing of low-confidence cases".

## 1. Purpose and success criteria

**Problem.** The engine is deterministic and has no confidence value, the AI is not allowed to judge, and routing by "model confidence" would need a trained model whose use in the decision path the mentor has not yet answered. Yet reviewers already produce the best evidence of whether a finding is trustworthy: they confirm it or override it.

**Goal.** Close the loop. What reviewers decide becomes a measured, per-rule **reliability**; reliability changes how later claims are routed; the cases the system knows least about are put in front of reviewers first so it learns faster (active learning).

**Success looks like** (each one is tested):

1. A rule that reviewers keep overriding is measurably flagged *doubtful*, and later claims with a finding from it go to a senior.
2. Cases from rules with the widest uncertainty are reviewed first, and in a fixed-budget simulation this shrinks the uncertainty faster than reviewing in arrival order.
3. No verdict, status, rule, evidence or AI output ever changes. Results are byte-identical with the feature on or off.
4. Every estimate is versioned, each receipt names the version it used, and the estimate can be re-derived and checked.
5. With the feature off (the default) every existing test, receipt and evidence file is unchanged.
6. One reviewer cannot swing a rule's estimate on their own.

**Not in scope.** A trained model of any kind (logistic regression, boosted trees): see section 3. Showing reliability to reviewers (anchoring risk). Changing the 15 rules or the triage formula. Real-reviewer accuracy: our evidence uses simulated reviewers and will say so.

## 2. Assumptions (what was said, what is assumed)

Said by the user: focus on this item first because it settles the human-in-the-loop gap; the bonus is worth 2 points; the problem is "confidence".
Assumed: no trained model is needed; a person who dismisses a finding is the "override"; a "senior" is the right escalation for a doubtful finding (it is the existing mechanism for "a person who may decide high-severity work"); everything is off by default.

## 3. Approaches considered

| Approach | Verdict |
|---|---|
| **A. Measured rule reliability from overrides, with uncertainty-first ordering** | **Chosen.** Counts and exact intervals only. Deterministic, explainable, replayable. No trained model, so it needs nothing from the mentor |
| B. A trained model (logistic regression or boosted trees) predicting "a human will uphold this finding" | Later. Needs real decision data we do not have. Shadow mode already records such predictions without acting; promoting one is a separate decision once data exists and the mentor allows it |
| C. Only report the numbers (the feedback report we already have) | Not enough: the loop is not closed, nothing changes routing |
| D. Ask the language model how sure it is | Rejected: poorly calibrated, and the AI must never influence routing |

## 4. Definitions

**Counted decision.** For each *decided* dossier and each flagged rule, the **last resolving action** (`confirm_issue` or `dismiss_with_reason`) across rounds, with its actor and time. `confirm_issue` is *upheld*; `dismiss_with_reason` is *overridden*. Information requests and "corrected" marks are not counted.

**Reviewer cap.** For a rule with `N0` counted decisions, one reviewer's counted decisions are limited to `max(1, floor(reviewer_cap * N0))`; their earliest by (time, claim id, version) are kept. This stops one person from dominating a rule.

**Per-rule statistics** over the capped decisions: `upheld` k, `n`, distinct `reviewers`, `rate = k/n`, an exact two-sided 95 % interval `[lower, upper]` (Clopper-Pearson, the routine already used by shadow mode), and `width = upper - lower` (1 when `n = 0`).

**Status of a rule.**

- `unmeasured`: `n < reliability_min_decisions` or `reviewers < reliability_min_reviewers`.
- `doubtful`: measured, and `upper < doubtful_below` (we are 95 % sure that fewer than `doubtful_below` of its findings are upheld). Using the *upper* bound means a small sample never causes doubt.
- `measured`: anything else.

**Claim uncertainty** = the largest `width` among the claim's flagged rules (0 when nothing is flagged).

## 5. Behaviour

### 5.1 Learning (the feedback loop)

`learn(store, cfg, now)` reads the decided dossiers, computes the per-rule statistics and returns the next **reliability snapshot**. If anything changed from the latest snapshot (counts or status) and learning is enabled, it is stored as the next version and `reliability_updated` is written to the security log with the number of rules whose status changed.

A snapshot holds: `version`, `created_at`, the parameters used, per rule `{upheld, overridden, n, reviewers, rate, lower, upper, width, status}`, the count of decisions considered, and `inputs_hash` (a hash of the sorted counted decisions). It holds **no claim id, badge or reason**.

**Verification.** `verify` recomputes rate, interval, width and status from each rule's stored counts and the stored parameters. A snapshot whose numbers do not follow from its counts is reported as altered.

**When it runs.** A scheduled task `learn` (every 600 s) runs it when `learn_enabled`; an administrator can also run `queue_admin learn` (a dry run that prints the proposed snapshot; `--apply` stores it).

### 5.2 Routing (using what was learned)

Only when `route_by_reliability` is on. At intake, after the normal receipt is computed, the latest snapshot is applied:

- If any flagged rule is `doubtful`, eligibility becomes `decide_high` (a senior must take it). The lane and the score do not change: the published formula is untouched.
- The receipt gains `reliability_version`, `doubtful_rules` (sorted list) and `uncertainty`.

A doubtful finding of medium severity does **not** trigger the two-person sign-off: that is reserved for high-severity findings. One senior decides it.

Routing is fixed at intake: a later snapshot changes later claims, not claims already waiting.

### 5.3 Active learning (reviewing the unknown first)

The dispatcher's priority becomes `score + aging_per_hour * hours_waited + uncertainty_weight * uncertainty`. With `uncertainty_weight = 0` (default) nothing changes. The deal snapshot stored for replay includes the uncertainty, so replay and `verify-deal` stay exact (deals written before this change read as 0).

### 5.4 Visibility

Administrators see the snapshot through `GET /api/v1/queue/reliability` (`queue.view`) and `queue_admin reliability`. **Reviewers are not shown reliability**: seeing "this rule is usually dismissed" would anchor their judgement and corrupt the labels the loop learns from.

## 6. Configuration (versioned, validated, default off)

Seven new fields of `RoutingConfig`, written through the existing versioned configuration (stale edits refused):

| Field | Default | Range |
|---|---|---|
| `learn_enabled` | false | boolean |
| `route_by_reliability` | false | boolean |
| `reliability_min_decisions` | 20 | 5 to 1,000 |
| `reliability_min_reviewers` | 3 | 1 to 50 |
| `reliability_reviewer_cap` | 0.4 | 0.1 to 1.0 |
| `doubtful_below` | 0.6 | 0.05 to 0.95 |
| `uncertainty_weight` | 0.0 | 0 to 20 |

Configuration documents written before this change load with these defaults.

## 7. Components and interfaces

| Unit | Responsibility | Depends on |
|---|---|---|
| `workqueue/reliability.py` (new) | `compute(decided_docs, params)`, `learn`, `apply(receipt, results, snapshot, cfg)`, `verify`; pure except `learn` | `shadow.clopper_pearson`, `triage` constants |
| `routing_config.py` | Seven new validated fields | none |
| `store.py` and `store_mongo.py` | `put_reliability(doc, expected_version)`, `latest_reliability()`, `reliability_history()`; new collection `reliability` with a unique `version` index | existing store contract |
| `intake.py` | Loads the latest snapshot and applies it when routing is on | `reliability` |
| `dispatcher.py` | Adds the uncertainty term to priority and to the deal snapshot | none |
| `tasks.py` | Scheduled `learn` task | `reliability` |
| `api.py` | `GET /queue/reliability`; `ConfigBody` accepts the new fields | `reliability` |
| `scripts/queue_admin.py` | `learn [--apply]`, `reliability`, `verify-reliability <version>` | `reliability` |
| `scripts/learning_experiments.py` (new) | The simulations in section 9; writes `outputs/defense/learning.json` | all of the above |

`reliability.py` does not import the API, the dispatcher or the AI step. Triage's `make_receipt` is **not modified**.

## 8. Failure behaviour

| Situation | Behaviour |
|---|---|
| No snapshot yet, or the store call for it fails | Routing proceeds exactly as if the feature were off; nothing is blocked |
| A snapshot fails `verify` | It is ignored for routing, a security event is written, and `GET /queue/reliability` and `queue_admin reliability` mark it as altered |
| Fewer decisions or reviewers than the minimum | The rule is `unmeasured` and never doubtful |
| One reviewer dismisses everything | The cap and the minimum-reviewers rule stop that from marking a rule doubtful (tested) |
| `learn` runs twice on the same data | The second run changes nothing and writes no version |
| Two `learn` runs race | The unique `version` index lets one win; the other is refused cleanly |
| The configuration is changed mid-run | Each receipt and each snapshot records the version it used |

## 9. Evidence we will produce (all simulated, all seeded)

1. **Convergence.** A pool of simulated reviewers with known upheld probabilities (most rules about 0.97; R006 0.35; R013 0.5): how the estimate, interval and status evolve, and after how many decisions R006 becomes doubtful.
2. **Routing effect.** The share of claims sent to seniors with routing off and on; results compared hash by hash to prove no verdict changed.
3. **Active learning gain.** With a fixed budget of reviewed claims, uncertainty-first ordering against arrival order: mean interval width, and decisions needed until every rule is measured.
4. **Poisoning.** One reviewer dismissing everything among five, with and without the cap and the minimum-reviewers rule.
5. **Invariants.** Receipts with the feature off equal the baseline for the 600 public claims; a tampered snapshot is detected by `verify`; replay of a deal with an uncertainty term still matches.

State plainly in the document: the reviewers are simulated, so this shows the mechanism and not real-world accuracy.

## 10. Testing

Unit tests for the statistics against known intervals; the cap; status transitions; determinism under any order of decisions. Property tests (`lower <= rate <= upper`; counted decisions never exceed the total; the snapshot is identical for any permutation of the input). One store contract suite run on the in-memory twin and on MongoDB for the three new store methods. Intake with the feature off, on, with no snapshot, and with an altered snapshot. Dispatcher priority and replay. The API permission matrix and body strictness. The operator tool. The scheduled task in eager mode. Mutation tests for the new code, all of which must be caught. The full suite must pass with MongoDB and Redis required, and the existing evidence files must not change.

## 11. Documentation

`docs/34_Active_Learning_Feedback_Loop.md`; a SPECS.md section 10h; the README Phase 2 table and briefing; `SPECIFICATION.md` (FR-22 and the human-in-the-loop line, a new requirement, a new use case "Learn from overrides" and the changes to UC12 and UC20, the data model and component diagrams). Evidence in `outputs/defense/learning.json`, added with `git add -f`.

## 12. Open question for the mentor (does not block this work)

Is a *trained* model ever allowed in the decision path? This design does not use one. If the answer is yes, approach B becomes a follow-up, trained on the decisions this loop collects.
