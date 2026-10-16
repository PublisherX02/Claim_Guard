# 51 | The reviewer copilot, what-if, and correct-and-recheck

This is the step the mentor's workflow needs between "the officer decides" and "the claim is revalidated": the reviewer sees what is wrong, what to ask for or correct, tries a correction without committing to it, and then saves it as a new version that is checked again. Everything is traceable and nothing is invented. It answers the mentor's email of 2026-10-16 points 3 and 8 (explainable findings, correction assistance, what-if validation, a reviewer summary, a traceable correction and revalidation).

## 1. What the reviewer gets

On a claim in their inbox (console, "Reviewer copilot" card, "Correct and recheck" card):

1. **A summary.** "2 finding(s) need attention: 2 high, 0 medium; 0 could not be assessed", then one sentence per finding in the order that clears the claim fastest (values that can be corrected from the claim itself, then values to check against the source, then information to request; high severity first), then the steps in order. A claim with no finding says "Pre-validation clearance: all 15 checks completed and none found a problem. This is not payer approval or a promise of payment."
2. **One suggestion per flagged finding**, of one of three kinds: *a value to correct*, *check against the source*, *ask the provider*. Each carries the rule, the claim paths it cites, and, only where it can be worked out, a proposed edit with its basis.
3. **What-if.** The reviewer edits any editable field (or presses "Try this in a what-if" on a proposal); the engine runs on a copy; the screen shows each rule before and after (cleared, still flagged, **new problem**) and the copilot summary of the corrected claim. A what-if stores nothing and changes nothing; only the security log notes that it happened.
4. **Recheck.** With a reason and a tick that the values were checked against the source, the correction is saved as a **new version** of the claim, which goes through intake and triage like any claim. The original version is kept exactly as received and closed (decided, then rechecked). The findings the correction cleared are recorded on the original as `mark_corrected_for_recheck` with the reviewer's reason. The new version shows who corrected which version, which fields changed, and why.

## 2. What it will not do (the grounding rules)

- **No model.** Summaries and suggestions are built by code from the engine's findings and the claim's own values (`rust/crates/cg-queue/src/copilot.rs`). Every sentence names a rule and the claim paths it rests on, so there is nothing for a model to get wrong. The AI-drafted explanation of docs/39 still exists beside it, behind the same grounding guard, and still cannot change a status.
- **No invented value.** A value is proposed only when it follows by arithmetic from other values on the same claim (a line amount from quantity times unit price; the claim total from the line amounts, also on the lines as corrected) or is the policy's own constant (the currency). A missing identifier, authorization, document, date or diagnosis is a request for information, never a guess.
- **No identifier is editable.** Patient, member, claim, line, document and authorization-record ids, notes and free text are refused (4,200 attempts across the 600 claims, all refused). Reviewers see pseudonyms; the correction is applied to the real claim on the server and the answer is masked again and checked for leaks before it leaves.
- **Correcting away a high-severity finding needs the authority to decide it.** A junior can try anything in a what-if, but a recheck that clears a high-severity finding needs the senior permission (`claims.decide_high`); otherwise correcting the data would be a way around the senior sign-off. Administrators and trainees have no part in it.
- **A claim in countersign cannot be corrected** (it is decided as it is), and only the person who holds the claim's lease can try or apply a correction.
- **A correction that recreates an earlier version is refused**, as is one that changes nothing.

## 3. Audit trail of one correction

| Where | What is recorded |
|---|---|
| Security log | `claim_whatif` (badge, claim, version, number of changes); `claim_rechecked` (badge, claim, prior version, new version, findings cleared, number of changes); `triage_receipt` for the new version |
| Review log (hash-chained, signed) | one `mark_corrected_for_recheck` per cleared finding, with the reason; a `recheck_run` event linking the two versions by input hash |
| Old version | state `decided` then `rechecked` (event `corrected_for_recheck`, naming the new version); the claim and results untouched |
| New version | `corrected_from`: version, badge, time, reason, every change as path, from and to, and the findings cleared. Sealed (encrypted) in MongoDB with the claim and the results |
| Trace panel (`GET /api/v1/ops/trace/{id}`) | both versions side by side with their events and decisions |

## 4. Interfaces

- `POST /api/v1/work/claims/{id}/whatif` with `{"changes": [{"path": "/lines/0/net_amount", "value": 180}]}` returns `{stored: false, applied, diff, claim, findings, copilot}`.
- `POST /api/v1/work/claims/{id}/recheck` with `{"changes": [...], "reason": "...", "rule_ids": [...optional]}` returns `{prior_version, new_version, diff, lane, score, still_flagged, cleared_for_recheck}`.
- The inbox view carries `copilot` ({summary, suggestions}), `can_correct` and, on a corrected version, `corrected_from`.
- Permission: `claims.recheck` (reviewers and above). Editable paths: the header fields, the coverage status and dates, and the line, authorization and attachment fields listed in `copilot::editable`.
- The older `POST /api/v1/claims/{id}/recheck` of the read-only file store stays 501 on purpose: the file store has no versions. The queue route above is the recheck.

## 5. Measurements (600 public claims, real engine)

`cargo test -p cg-queue --test copilot_eval -- --nocapture` writes `outputs/defense/copilot_eval.json`.

| Measurement | Result |
|---|---|
| Flagged findings | 777, each with exactly one suggestion and a text |
| Suggestions by kind | 98 a value to correct, 222 check against the source, 457 ask the provider |
| Cited paths that do not exist on the claim | 0 |
| Suggestion texts with approval or payment language | 0 |
| Proposed correction clears the finding it was proposed for | R007 30 of 30, R012 34 of 34, R015 29 of 29 |
| A proposed correction creates a new flagged rule | 0 (an earlier version of the copilot proposed the line amount alone and created a new R012 failure in 25 of 30 cases, because the claim total had matched the wrong line; the what-if measurement found it and the copilot now proposes the line and the total together) |
| Claims with a proposed correction | 88, of which applying all proposals clears every finding on 53 and leaves the rest with findings that need information |

Limits. The public claims are synthetic. "Clears the finding" means the rule passes, not that the corrected value is true: a reviewer still checks the figure against the bill, and the copilot says so on screen. Only three rules have computable proposals; the other twelve produce requests and checks, by design. A browser run (`rust/tools/correction_e2e.py`, 11 checks) walks the whole flow in Chrome: a wrong claim arrives, the copilot names R007 and R012, the what-if clears both and saves nothing, the recheck creates version 2, the trace shows both versions, and the security log holds the what-if and the recheck.

## 6. Tests

`cargo test -p cg-queue --test service_flow` (four new tests): a what-if changes nothing and refuses identifiers, notes, nonexistent fields, no-ops and non-scalar values, and cannot be run by someone who does not hold the claim; a recheck makes version 2, closes version 1, preserves the original claim byte for byte, writes the review-log events and the security events, and cannot be sent twice; a junior may what-if but not recheck away a high-severity finding and a refused recheck stores nothing; a reason is required and may not contain the patient identifier.
