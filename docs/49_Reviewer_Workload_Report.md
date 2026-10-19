# 49 | Where reviewer time goes: the workload report and what it could save

Command: `cg queue workload [--skip-lower X]` (read-only, needs `queue.view`). Study: `cg sim workload` writes `outputs/defense/workload.json` (20 seeds). Code: `rust/crates/cg-queue/src/workload.rs`.

**Read this first: the study uses simulated reviewers.** Each rule has a hidden true upheld rate that we chose (97% for most, 35% for R006, 50% for R013); reviewers answer from it with bias and noise; claims and findings are the real development set, recycled. It scores the report against a truth that real data does not have. No real reviewer decision was used, and nothing here measures time (see section 4).

## 1. Why this exists

The goal is less work for reviewers and administrators. The earlier build of this idea (`docs/48`, now archived and not wired) sent more claims to seniors and added an approval step for administrators; in the simulation the senior share went from 76% to 88%. It added oversight. It saved nothing. This report points the same measurements at work that can be removed.

## 2. What the report says

It reads the decided claims and, per rule, counts how many findings were upheld and overridden (same counting, caps and exact 95% intervals as `docs/48`), then sorts each rule:

- **retune**: we are 95% sure fewer than 60% of this rule's findings are upheld. Every overridden finding is a review that produced nothing. The list is for a person to look at the rule, the training or the payer policy; the report changes nothing.
- **light review**: we are 95% sure at least `skip_lower` (default 0.95) of the findings are upheld. These are the candidates for a lighter look. The report states how many wrong findings that skips (estimate, and an upper bound that holds with 95% confidence), because that is the price.
- **full review**: everything else, including every rule with fewer than 20 decisions or fewer than 3 reviewers.

It asks no administrator for anything: no snapshot, no replay, no approval, no routing change. A person decides what to do with the lists.

## 3. Evidence (simulated, 20 independent histories, seeds 1 to 20)

**Finding rules that waste reviews.** The two truly bad rules (R006, R013) are listed as "retune" in:

| Decisions seen | 300 | 600 | 1,200 | 2,500 | 5,000 |
|---|---|---|---|---|---|
| Share of the bad rules found (mean over 20 histories) | 0% | 55% | 72.5% | 90% | 100% |
| Good rules wrongly listed per history | 0 | 0 | 0 | 0 | 0 |
| Overridden reviews on the listed rules, as share of all findings | 0% | 5.3% | 6.6% | 8.7% | 9.6% |

So in this setting, once about 600 decisions exist the report is half-way there, with no false alarms at any size; by 5,000 it found both rules in all 20 histories. The last row is what removing the overridden reviews would save: about one finding in ten. That figure depends on our having made two rules bad; it says what the report can find, not that real rules are bad.

**Finding findings that can get a lighter look.** `skip_lower` is the least lower bound of the upheld rate. "Wrong" means a finding the hidden truth says should not be upheld. Mean over the histories that had any candidate:

| skip_lower | Decisions | Histories with a candidate | Findings covered | Truly wrong among them | Stated 95% bound | Bound held |
|---|---|---|---|---|---|---|
| 0.85 | 1,200 | 100% | 74% | 26.8 | 92.9 | 100% |
| 0.85 | 5,000 | 100% | 82% | 124.6 | 278.7 | 100% |
| 0.90 | 1,200 | 100% | 34% | 12.3 | 34.2 | 100% |
| 0.90 | 5,000 | 100% | 82% | 123.4 | 274.5 | 100% |
| 0.95 | 2,500 | 50% | 4% | 6.1 | 8.9 | 100% |
| 0.95 | 5,000 | 75% | 8% | 16.1 | 23.8 | 100% |

The stated bound on wrong findings held in every history, and no candidate rule had a true rate below 0.9 at any setting. The trade is visible: a looser threshold covers far more findings (82%) but lets through more wrong ones (about 125 against a bound of 279 over 5,000 decisions); the strict 0.95 threshold is safe but covers only 8% because simulated reviewers' own mistakes (2% to 15% random answers, up to 5% bias) keep even a 97%-correct rule's observed upheld rate near 0.95. Real reviewers' noise is unknown, so the real coverage at 0.95 may be better or worse.

What a wrong finding that skips full review means: it would be confirmed without anyone catching that it should have been dismissed, so a valid claim goes back to the provider for a correction it does not need. That is a cost to the provider and to trust in the system, not a missed fraud. Whether that is acceptable for any rule is a policy decision; the report only prices it.

## 4. What is not measured

- **Time.** Simulated timestamps are invented, so the study reports counts and shares only. The production report measures seconds per finding from the queue's own lease and decision times (dossiers held over the lease length are left out) and multiplies by the counts to give hours. Until real review has happened, that field says it has no data.
- **Real rules.** On the 600 labelled claims the engine matches the organizers' answer key (F1 1.0, `docs/46`), so on that data no rule is "wrong". If real reviewers override a correct finding often, the remedy is training or a clearer payer policy, not changing the rule. The report cannot tell these apart; a person reads the overridden reasons.
- **Drift.** A rule's rate describes the claims seen so far. If the mix changes the report must be re-run; it is cheap and changes nothing.
- **Real reviewers.** Everything above depends on how well the simulation resembles them, which is unknown.

## 5. Open question for the mentor

Is "reviewers almost always agree, so a lighter look" an acceptable way to cut review time in this domain, and at what residual error? And should the earlier idea of routing doubtful rules to a senior (`docs/48`, archived) be built at all, given that it adds senior work?

## 6. Claims processed with no person, on random batches of 500

`cg sim automatic` (evidence `outputs/defense/automatic.json`): 20 random batches of 500 claims drawn without replacement from the 600 labelled claims (development, validation, stress). Because the pool is only 600, batches overlap by about 83%, so the min-max ranges understate real batch-to-batch variation. The split into green and flagged claims is real (the engine matches the answer key on all of them, `docs/46`). Which flagged rules count as "almost always upheld", and whether an automatic flagged claim was wrong, come from simulated reviewers (3,000 simulated decisions per batch). The batch mix is the organizers' synthetic one, not a hospital's.

| Policy | Processed with no person (mean, range) | Left for a person | Findings a person still reviews | Automatic claims that were wrong |
|---|---|---|---|---|
| Today | 0 of 500 | 500 | 645 | 0 |
| Green claims only (no flagged finding) | 167 (160 to 179) = 33% | 333 | 645 | 0 on this data |
| Green + light-review rules, `skip_lower` 0.95 | 174 (160 to 202) = 35% | 326 | 638 | 0.2 (0 to 2) |
| Green + light-review rules, `skip_lower` 0.90 | 358 (302 to 393) = 72% | 142 | 420 | 6.5 (3 to 12) |
| Green + light-review rules, `skip_lower` 0.85 | 388 (378 to 393) = 78% | 112 | 370 | 8.3 (4 to 14) |

Reading it:
- The 33% from green claims is the firm part: no flagged finding and no degraded result, nothing for a person to decide. It is the share of the dataset's claims that are valid, so a real queue would differ.
- The extra 2% to 45% comes from flagged claims whose every finding is in a rule reviewers almost always uphold. It depends on simulated reviewers and on the threshold. The looser thresholds process 72% to 78% of a batch automatically at a cost of about 7 to 8 claims per 500 (roughly 2% of the automatic claims) that a person would have corrected: each is a valid claim sent back for a needless correction.
- Under today's rules green claims are still verified by a person; automatic processing of green claims is a policy change the mentor has not been asked about.

## 7. Where dismissals concentrate (feedback report, `by_service`)

The feedback report (`/api/v1/queue/feedback`, console "Queue overview") now splits each rule's final confirmed and dismissed counts by the **service code and modifier** of the line the finding pointed at (`SVC-DENTAL | EDU-SEPARATE`; findings with no affected line go under `(claim level)`). It answers the mentor's "learning from feedback: identify rules producing frequent false alarms" at a finer grain than the rule: a rule that is fine overall can still be dismissed almost every time on one service.

Counts only (confirmed, dismissed, decisions, dismissal rate): no interval and no review-candidate flag per cell, because a cell is small and is a pointer for a person to look at, not a verdict. No claim id, badge id, patient value or dismissal reason is included; labels are cut at 40 characters. The system's own decisions (auto-clear, auto-request) are not counted, as before. Code: `src/workqueue/feedback.py` and `rust/crates/cg-queue/src/service.rs` (`service_labels`); tests `tests/test_wq_feedback.py` and `consumer_tests.rs::the_feedback_report_splits_decisions_by_service_code_and_modifier`. Like the rest of this document, real data is needed before the cells mean anything; the simulation only checks the arithmetic.
