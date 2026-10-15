# 48 | Human-in-the-loop confidence routing and the learning loop

Design: `docs/superpowers/specs/2026-10-14-hitl-confidence-and-learning-design.md`. Plan: `docs/superpowers/plans/2026-10-14-hitl-confidence-and-learning.md`. Evidence file: `outputs/defense/learning.json` (re-create with `cg sim experiments`).

**Read this first: the reviewers in every experiment below are simulated.** Their answers follow a hidden "true upheld rate" per rule that we chose (97% for most rules, 35% for R006, 50% for R013). The claims and the engine's findings are real (the 400-claim development set), recycled under new ids to reach thousands of decisions, so findings repeat. The numbers show how the mechanism behaves. They are not the accuracy the system would have with real people, and no real reviewer's decisions were used.

## 1. What problem this solves

The brief asks for routing by confidence and for the system to learn from reviewer overrides. The rule engine is deterministic and has no probability of being right, so "confidence" here is not a model score. It is a measured quantity: of the findings a rule has raised, what share did reviewers uphold (confirm) rather than override (dismiss), with an exact 95% interval.

## 2. How confidence is defined

- Source: the last resolving action (confirm = upheld, dismiss = overridden) of each flagged rule on each claim. Information requests, rules that were not flagged on that claim, and a second resolution of the same rule are not counted.
- Per rule: counts, rate, Clopper-Pearson interval (checked against the Python oracle on 1,685 cases, largest difference 2.8e-13: `rust/tools/compare_reliability.py`).
- One reviewer can supply at most 40% of a rule's decisions (`reliability_reviewer_cap`), and a rule needs at least 20 decisions from at least 3 different reviewers before it is judged (`unmeasured` until then).
- A rule is **doubtful** when we are 95% sure that fewer than 60% of its findings are upheld (interval upper bound strictly below `doubtful_below`).

## 3. What it changes

Everything is off by default (`learn_enabled`, `route_by_reliability`, `uncertainty_weight = 0`). With them off no stored receipt gains a field.

- **Routing (`route_by_reliability`):** a claim with a finding of a doubtful rule needs a senior (`decide_high`), even if all its findings are medium severity.
- **Order (`uncertainty_weight`):** the dispatcher adds `weight x uncertainty` to a claim's priority, where uncertainty is the widest interval among its flagged rules (a rule never seen counts as 1.0), so the least-known cases can be reviewed earlier.
- **Never:** no verdict, status, evidence, rule or AI text changes. Only `receipt.eligibility` and the dealing priority can move.
- **What reviewers see:** plain words only (`high_severity`, `low_confidence`, `could_not_assess`, `degraded`, `disagreement`) as badges. No reliability number reaches the inbox view.

## 4. Governance: a learned change is a proposal until two administrators agree

1. `learn` (worker job every 10 minutes when enabled, or `cg queue learn --apply`, or the Learning panel) writes a numbered **proposed** snapshot with a **replay report**: the latest 500 claims re-routed on their stored results, showing how many need a senior before and after, and that no verdict hash changes. A rise in senior load is stated in words.
2. An administrator can **re-derive every number** (`verify`): each figure is recomputed from the stored counts and compared.
3. **Activation** goes through the same dual-control gate as payer rules: with two or more active administrators a different one must approve (with their authenticator code); a lone administrator acts alone and the security log says so. A proposal that is not the newest, or not `proposed`, cannot be activated.
4. Routing ignores a snapshot that fails `verify` (altered counts, statuses or parameters) and records `reliability_snapshot_rejected`.
5. Status moves (`proposed` to `approved` / `rejected`, `approved` to `superseded`) are single conditional updates in the store: of six administrators approving at once exactly one wins; of six learners racing for a version exactly one stores it (memory and MongoDB, same contract test).

Routes (administrators only, level 4): `GET /api/v1/queue/reliability`, `POST .../learn`, `GET .../{v}/verify`, `POST .../{v}/activate`. Console: "Learned reliability" page. Operator commands: `cg queue learn [--apply] | reliability | verify-reliability V | replay-reliability V | activate-reliability V`.

## 5. Failure behaviour

| Situation | Behaviour |
|---|---|
| No decisions yet / too few | nothing proposed; rules stay `unmeasured`; routing unchanged |
| Claim with no flagged finding | uncertainty 0, no badges, never routed by reliability |
| Rule absent from the snapshot | uncertainty 1.0 for that claim, not doubtful |
| Snapshot altered after approval | refused by `verify`, ignored for routing, security-log event |
| Stale or already-used proposal activated | refused (not proposed: 409; older than the active one: 422); no duplicate version |
| Two learners at once | one stores the version, the other gets nothing |
| Store unavailable at intake | routing behaves as if the feature were off |

## 6. Evidence (simulated reviewers; seed 1)

**Convergence.** R006 (true rate 0.35) first reads *doubtful* after 405 counted decisions, never before the 20-decision minimum. At 505 decisions its interval is 16% to 49% (rate 30%); at 4,039 it is 28% to 40%, containing the hidden 35%. R013 (true 0.50) becomes doubtful by 505 decisions. No rule with a hidden true rate of 0.97 was ever marked doubtful at any checkpoint.

**Routing effect and senior load.** Learning from 2,000 decisions, then reviewing 793 further claims: with routing on, the share needing a senior rises from 76.2% to 87.5% (604 to 694 claims; from 151 to 173.5 claims per senior, with four simulated seniors), 90 claims newly routed to a senior, because R006 and R013 findings are common. **Routing by reliability therefore increases senior workload**; that is a cost the administrators see in the replay report before approving, and `doubtful_below` can be lowered to limit it. All 793 verdict hashes are identical with routing on and off.

**Reviewing the least-known cases first.** On the simulated data this was not a clear win: mean interval width at 600 decisions was 0.178 with uncertainty-first ordering against 0.192 in arrival order, but every flagged rule was "known" after 968 decisions against 926. Treat it as a demonstration of the mechanism, not a finding.

**Poisoning (one adversarial senior who dismisses everything, deciding a share of all claims).** Rules wrongly marked doubtful (of 15; true rate 0.97), without and with the protections (cap and 3-reviewer minimum):

| Adversary share | 10% | 20% | 30% | 40% | 50% | 60% | 80% | 100% |
|---|---|---|---|---|---|---|---|---|
| Without protection | 0 | 0 | 9 | 11 | 13 | 13 | 13 | 13 |
| With protection | 0 | 0 | 0 | 8 | 10 | 13 | 13 | 0 |

The protections stop a minority adversary up to about 30% and a fully captured queue (the 3-different-reviewers minimum keeps every rule unmeasured). **They do not stop an adversary who decides 40% to 80% of the claims**: the cap limits one person to 40% of a rule's decisions, which is still enough to flip a rule when the honest reviewers are a minority. That is why activation needs two administrators and a replay report: a bad snapshot is visible before it is used.

**Invariants (all true):** with the feature off, receipts equal the plain receipts and carry no reliability field; a snapshot with one altered status fails `verify`; applying a snapshot changes none of lane, score, result hash, facts hash, statuses or degraded flag over 200 claims; a deal made with the uncertainty term replays from its stored seed.

## 7. Limits and open questions

- Everything above is simulated. Real reviewers disagree for reasons unrelated to rule correctness (workload, training, case mix); a low upheld rate can mean a bad rule or a bad week. The system measures override rates, not truth.
- Label shift: if the claim mix changes, an old snapshot describes the old mix. Snapshots are numbered and superseded, not edited, and `learn` can be run again at any time.
- Learned reliability is counts and intervals, not a trained model. A model (for example logistic regression on claim features) was not built.
- Open for the mentor: is routing a doubtful rule to a senior the intended "low confidence" handling, given that it raises senior load (section 6)?

## 8. How it is tested

Unit tests in `cg-queue` (statistics, cap, last-resolution rule, tamper cases, learn/replay/activate, uncertainty in dealing), the store contract on memory and MongoDB with thread races, API tests (`cg-api/tests/reliability_flow.rs`: permissions, lone-administrator and two-administrator activation), service tests (reviewers see words only), the `cg-sim` experiments (9 tests), and mutation tests for `reliability.rs` (`rust/tools/mutate.py`, ids starting `rel-`).
