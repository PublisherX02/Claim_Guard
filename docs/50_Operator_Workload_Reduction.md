# 50 | Reducing the operators' work: what was built, what was measured, what is still unknown

Plan: `docs/superpowers/plans/2026-10-15-operator-workload-reduction.md`. Measurements it builds on: `docs/49` (rule reliability, light review, 500-claim batches). Related: the archived confidence routing (`docs/48`).

**Read this first.** Nothing here has been measured on real reviewers. The share of green claims comes from the organizers' synthetic mix; "almost always upheld" rules, in the studies of docs/49, came from simulated reviewers; no review time has been measured at all. The new report that measures time exists and says "null" until real timestamps exist. The three changes that alter who looks at what are **off by default** and wait for the mentor.

## 1. Where an operator's time goes today

Per 500 random claims (docs/49 section 6): about 167 green claims (33%) that a person still verifies, and about 645 findings that a person decides one by one. Every decision, even a plain confirm, used to need a typed reason. Those three things are where time can be taken out.

## 2. What happened to Laya (the fine-tuned model)

Laya (`convaiinnovations/laya`, Apache 2.0, 421M parameters) was fine-tuned on Kaggle (2x T4, 6 epochs, about 41 minutes) on 2,486 real labelled claim-rule cases, with no simulated reviewers. Package and instructions: `experiments/laya_kaggle/`.

| Test split, 150 unseen claims | Accuracy | Macro-F1 | ECE |
|---|---|---|---|
| Base Laya, zero-shot | 0.631 | 0.289 | 0.276 |
| Fine-tuned | 0.971 | 0.915 | 0.027 |
| Always guess PASS | 0.835 | - | - |

It learned the rulebook and is well calibrated, but it recovers only 64% of real FAILs (the rest are called PASS), and the labels are the engine's own answer key. Run after the engine it can only re-predict what the engine already computed exactly, so it cannot save an operator time: when it agrees it adds nothing, and when it disagrees it is almost always the one that is wrong. **It is archived as an experiment, not wired.** A model is worth revisiting only when real operator decisions exist, and then the first thing to try is a plain per-rule count (it may already do the job).

## 3. What was built

All settings are fields of the versioned routing configuration (`PATCH /api/v1/queue/config`; every change is in the security log with before and after).

### 3.1 One-click confirm (on, no policy change)

Confirming agrees with the engine, so it needs no typed reason: the server records the fixed text "Confirmed: the finding matches the evidence." Dismissing still needs a reason, because that overrides the engine. The console gains a **Confirm the issue** button on every finding and **Confirm all N findings** on a claim (two clicks, no browser dialog). The decision is the same record in the review log and the security log as before.

### 3.2 Auto-clear green claims with an audit sample (`auto_clear_green`, default **off**)

A green claim (nothing flagged and an intact set of fifteen results) is today verified by a person. With `auto_clear_green` on, the system decides it (actor `system:auto-clear`, field `auto_cleared`) unless a random draw puts it in the audit sample (`audit_percent`, default 5, 1 to 100), in which case it goes to a person as before. The draw is written on the claim (`audit_draw`), so each one can be checked. A claim that is damaged or has a flagged finding is never cleared by the system, whatever its lane says. System-cleared claims are not counted as verified by a person in the shadow and feedback reports.

`cg queue audit` reports how it is doing: claims cleared by the system, the audited sample (verified clear, escalated, still open), the escalation rate with an exact 95% upper bound, and the most auto-cleared claims that could carry a sign the engine missed. Every escalation is counted as a possible miss, so the bound is conservative.

How big the audit must be to say anything. With zero escalations among n audited claims, the 95% upper bound on the miss rate is 1 - 0.05^(1/n):

| Audited claims | 30 | 60 | 150 | 300 | 600 | 1,000 | 3,000 |
|---|---|---|---|---|---|---|---|
| Upper bound | 9.5% | 4.9% | 2.0% | 1.0% | 0.5% | 0.3% | 0.1% |

Measured on the real pipeline (`cargo test -p cg-queue --test auto_clear_run`, evidence `outputs/defense/auto_clear_run.json`): the 600 labelled claims, the real engine and triage, `auto_clear_green` on at 5% audit, 20 random batches of 500. The system cleared **159.1 claims per batch on its own (149 to 172, 31.8%)**, 7.7 green claims went to a person as the audit sample, 333.2 claims with findings stayed for a person, so a person handled 340.9 of 500. **0 of the cleared claims had a finding in the answer key** (and no green claim was left uncleared and unaudited). This is the organizers' synthetic mix and the engine matches the key exactly on these claims, so it says what the policy does here, not what a hospital would see.

At 5% audit, 6,000 green claims put 300 in the sample. The saving is the other 95%: on the 500-claim mix, about 158 of 500 claims need no person.

### 3.3 Bulk confirm for rules reviewers almost always uphold (`bulk_confirm_skip_lower`, default **0 = off**)

When set (0.8 to 0.999), a rule whose exact 95% lower bound of the upheld rate is at least that value (and has at least 20 decisions from at least 3 reviewers, `docs/49`) becomes a light-review rule. The inbox then has a **By rule** view; for a light-review rule the operator opens the list, looks at the findings, and confirms them all in two clicks. Guards, all server-side:
- the rule must be light-review right now (checked again on every request; the set is cached for five minutes);
- each claim goes through the ordinary decision path, so leases, clearance (a junior cannot confirm a high-severity finding), the two-senior countersign, masking and both logs all apply;
- only confirm is possible;
- a random `audit_percent` of the claims in each request is not confirmed but left for a full review;
- bulk decisions (recorded with the reason "Confirmed in bulk after a light review.") are **not** counted as evidence about the rule. Otherwise the rubber stamp would keep the rule eligible by itself and a rule that went bad later would never show it.

The "look at the list first" step is enforced by the screen only; the server cannot know that a person looked.

### 3.4 Where time really goes (`cg queue workload`, read-only)

The report now adds, from the queue's own timestamps only: seconds to verify a green claim, seconds to open a claim with findings (lease to first decision), seconds per finding (gap between decisions) overall and by rule. Bulk decisions and gaps longer than the lease are left out. From these and the queue's own mix it estimates hours per 500 claims today and the hours each saving would give: auto-clear, retuning wasteful rules, and bulk confirm if a light look takes 0, 25 or 50% of the normal time (an input, not a measurement). Any number whose inputs are missing is null.

## 4. How to get the first real numbers: a timed session

1. Load claims: `cg queue --badge CG-4004 submit --claims data/validation/claims.jsonl --process`.
2. Put the participants on shift (`on_shift` in the configuration) and let each review at least 50 findings in the console as they normally would, including some green claims. At least 3 people, so that rules can pass the minimum-reviewers test.
3. Run `cg queue workload` and read `time.breakdown` and `planning_per_500_claims`.
4. With auto-clear and bulk confirm still off, this measures today. Teammates are not trained operators and the claims are synthetic, so treat the seconds as a first estimate, not as a result.

## 5. Questions for the mentor

1. May a green claim skip a person at all, with a random audit that measures what the engine misses? At what residual miss rate (the table above)?
2. May an operator confirm many findings of a rule in one action after a light look, for rules reviewers almost always uphold? Is a random full-review share acceptable as the safeguard?
3. Does the answer to 1 change if the audit is run by a second person?

## 6. Limits, stated plainly

- No real review time exists yet; every "saving" above is a design with an estimator, not a measurement.
- One administrator (`routing.manage`) can switch auto-clear or bulk confirm on; the change is logged with before and after but needs no second approval. There is no automatic brake: if the audit starts finding misses, a person must read `cg queue audit` and switch the setting off.
- The audit measures only what a person notices when looking at a green claim for a few seconds. A violation the engine misses and a person also misses stays invisible.
- Bulk confirm leans on the same "reviewers almost always uphold" evidence as docs/49, which was studied on simulated reviewers.
- The hours for bulk confirm and retuning can overlap; they are independent estimates.
- The Python reference build (`src/`) does not have these routes: against it the Confirm button is refused (it still requires a reason) and bulk confirm is absent. The Rust server is the one these changes are for.
