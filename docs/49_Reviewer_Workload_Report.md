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
