# Operator workload reduction: plan (2026-10-15)

Goal (the user's words): the system must save operators time. A model is only worth keeping if it does. Laya fine-tuned on engine labels cannot do that (docs/50, section "What happened to Laya"), so it is archived and the time is saved where it is actually spent.

Where time goes today, per 500 random claims: 167 green claims (33%) each verified by a person; 645 findings each decided one by one; and every decision, even a plain confirm, needs a typed reason (`validate_decision`: "A reason is required for every decision").

All changes are additive. The ones that change who looks at what are **off by default** and need the mentor's answer before they are switched on.

| # | Change | Default | Why it saves time |
|---|---|---|---|
| 1 | `auto_clear_green` + `audit_percent`: a green claim is decided by the system (`system:auto-clear`) unless a random audit sample sends it to a person; `cg queue audit` reports the miss rate with an exact upper bound | off | removes the ~33% of claims that have nothing to decide |
| 2 | Confirm needs no typed reason (a fixed, identifier-free default is recorded); one-click Confirm and "Confirm all on this claim" in the console | on (it only removes typing; dismiss still needs a reason) | cuts per-finding time, no policy change |
| 3 | `bulk_confirm_skip_lower`: findings of rules reviewers almost always uphold (docs/49) can be confirmed in bulk by rule after a lighter look; server-enforced, confirm only, every normal rule (leases, countersign, masking) still applies | 0 = off | one action instead of N for the safest rules |
| 4 | Workload report adds seconds per finding by rule, claim-open overhead and green-verify time, and turns counts into hours for 0, 25 and 50% look time | on (read-only) | tells us where the time really goes, from the queue's own timestamps |
| 5 | Laya: archived as an experiment, documented | n/a | not the right tool for this goal |

Safety notes: the audit sample is a random draw logged on the claim; auto-clear never touches a degraded or flagged claim; the audit's miss upper bound is the price of the policy; there is no automatic brake (an administrator switches the setting off), which is stated as a limit.
