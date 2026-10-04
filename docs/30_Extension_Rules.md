# 30. Extension rules (advisory)

The mentor's rulebook has fifteen rules, R001 to R015, and we implement exactly those for the official verdict. This document covers
eight further checks, **E001 to E005 and E101 to E103**, taken from real claim-validation practice. They run in a separate pack, write
to a separate file with their own schema, and **never change the official results**. A test pins the official files and the official
results on all three public splits to the values recorded before this work started.

The ids start with `E` on purpose: the mentor's hidden rules are probably numbered R016 and up.

Design: `docs/superpowers/specs/2026-10-04-extension-rules-design.md`. Plan: `docs/superpowers/plans/2026-10-04-extension-rules.md`.

## What is real and what is invented

The rule **logic** follows published practice. The **attributes** the single-claim rules need are invented, because the teaching
catalogue has only six services and six diagnoses and no code-pair tables, drug flags or accident flags. They live in
`rules/extensions/catalogue.json`, labelled fictional, and use codes outside the teaching catalogue
(`SVC-EXT-PRIMARY`, `SVC-EXT-COMPONENT`, `SVC-EXT-NEVER`, `SVC-EXT-RX`, `DX-EXT-ACCIDENT`, `DX-EXT-SECONDARY`).

Why outside the catalogue: we measured the 600 public claims. All 15 pairs of the six real services occur together on one date in
36 to 54 claims each; 170 claims carry a pharmacy line and none has a route modifier; all six diagnoses are each used by 90 to 109
claims. A pair, route or accident rule on the real codes would have flagged dozens of claims the organizers consider valid. So
E001 to E005 cannot fire on public data by design, and are verified on constructed scenarios, an independent oracle and fuzzing.
E101 to E103 use the real codes and the official limits, but every public patient has exactly one claim, so they are verified on
constructed multi-claim scenarios.

The official R011 also flags the `SVC-EXT-*` codes as outside the official catalogue. That is correct: the extension rules describe
what a larger catalogue would need, not what the teaching catalogue contains.

## The rules

Status follows the official engine: a missing input gives `UNABLE_TO_ASSESS` (never `PASS`), an absent trigger gives
`NOT_APPLICABLE`. Modifier matching is exact (case and spaces count).

### E001 Procedure pair without exception modifier (medium)
**Logic.** Two lines on the same date whose service codes form a listed pair, where the component line carries no exception
modifier (`EDU-SEPARATE`). **Source.** CMS NCCI Policy Manual, chapter 1: each procedure-to-procedure edit has a column one and a
column two code; billing both pays only column one unless an allowed modifier is used. *Confirmed.*
**Invented.** The pair table. **False-positive risk.** Two truly distinct services on one date; the corrective action says to attach
the modifier and documentation. **Example.** `SVC-EXT-PRIMARY` and `SVC-EXT-COMPONENT` on 2026-03-02, no modifier: FAIL, both lines named.

### E002 Exception modifier on a never-bundle pair (high)
**Logic.** A pair marked `modifier_allowed: false` appears with an exception modifier anyway. **Source.** CMS NCCI: modifier
indicator 0 means no modifier may bypass the edit, indicator 1 means one may. *Confirmed.* **Invented.** Which pairs are never-bundle.
**Example.** `SVC-EXT-PRIMARY` plus `SVC-EXT-NEVER` carrying `EDU-SEPARATE`: E002 FAIL (and E001 PASS, since a modifier is present).

### E003 Event date required by the diagnosis (medium)
**Logic.** A diagnosis flagged `requires_event_date` needs `Event date: YYYY-MM-DD` in the notes or an attachment text, a valid
calendar date on or before the earliest service date. **Source.** WEDI SNIP level 4 (situational requirements): "if A is present,
B must be populated". *The pattern is confirmed; the pairing of an accident diagnosis with an event date is our fictional instance,
not a published edit.* **False-positive risk.** An event date written in another format. **Example.** `DX-EXT-ACCIDENT` with no
event date: FAIL; with `Event date: 2026-03-01` and a first service on 2026-03-02: PASS.

### E004 Route of administration missing (medium)
**Logic.** A line for a service in category `pharmaceutical` (`SVC-EXT-RX`) must carry a route modifier (`EDU-ROUTE-ORAL`,
`EDU-ROUTE-IV` or `EDU-ROUTE-TOPICAL`). **Source.** Dubai eClaimLink publishes a Route Of Administration coding set for drugs.
***Not independently confirmed:** the rule "a drug line must carry a route" comes from the research report and we found no primary
source stating it.* Keep it as an illustration, not as a claim about eClaimLink. The official `SVC-PHARM` is deliberately not covered.

### E005 Diagnosis cannot be primary (medium)
**Logic.** The header diagnosis is flagged `secondary_only` (`DX-EXT-SECONDARY`). **Source.** ICD-10-CM Official Guidelines: a
manifestation ("code first") code cannot be the first-listed or principal diagnosis; payer policies that quote them reject such
claims. *Confirmed.* **Invented.** Which fictional code plays the manifestation role.

### E101 Line duplicates an earlier claim (high)
**Logic.** A line equals a line of an earlier claim for the same patient and provider on service code, service date, quantity and net
amount (numbers compared by value, so 1 equals 1.0). **Source.** CARC 18, "exact duplicate claim/service". *Confirmed.*
**False-positive risk.** A genuinely repeated service on the same day; the reviewer decides.

### E102 Authorization exhausted across claims (high)
**Logic.** Units under one authorization id in earlier claims plus this claim exceed the authorization's `max_quantity`, when earlier
claims contributed units. **Source.** Cumulative authorization tracking as described for NPHIES; *not independently confirmed*; it
extends our R009. **Design note.** The first public-data run showed E102 flagging 8 claims that R009 already fails, because it also
counted a single claim's own excess. It now fires only when earlier claims contributed units, so R009 and E102 never double-report.

### E103 Daily quantity across claims (medium)
**Logic.** Units of one service on one date for one patient and the same provider, in earlier claims plus this claim, exceed the
service's `max_quantity` (from `rules/services.json`), when earlier claims contributed units. **Source.** CMS Medically Unlikely
Edits, date-of-service edits (adjudication indicators 2 and 3): units are summed per beneficiary, provider and date and compared
with the limit. *Confirmed.* It never overlaps R006 or R013 (within one claim) and takes no side on the `EDU-SEPARATE` policy
question raised for the mentor.

## "Earlier claim"

Same non-empty `patient_id`, a different `claim_id`, and ordered strictly before this claim by `(submission_date, claim_id)`. The
order makes a claim's result independent of how many later claims exist, so a batch run is reproducible. A corrected resubmission
that reuses a `claim_id` is never its own history. Without history, E101 to E103 report `UNABLE_TO_ASSESS`, and so does a history
store that returns anything other than a list of claims. The queue work will replace the in-memory history with a database-backed one.

## Running them

```
python scripts/run_extensions.py --claims data/development/claims.jsonl --out outputs/extensions/development.jsonl
python scripts/extension_experiments.py          # writes outputs/evaluation/extensions.json
```

## Evidence

`outputs/evaluation/extensions.json` (commit named inside it):

| Measurement | Result |
|---|---|
| Findings on the 600 public claims | 0 FAIL for every rule, as designed. E003/E005: 12 claims have no diagnosis (unable). E101/E103: 15 claims lack a provider or patient (unable) |
| Engine versus independent oracle on those claims | 4,800 of 4,800 rule results agree, with the same affected lines |
| Constructed scenarios, status fixed by construction | 29 of 29 agree (95% upper bound on the disagreement rate: 9.8%; a few dozen cases cannot prove a low rate) |
| Time per claim | median about 5 ms with 50 earlier claims for the patient and no other history; about 9 ms with 10,000 other claims in history (laptop, Windows, Python 3.10) |

## How it was tested

- An **independent oracle** (`tests/oracle_extensions.py`) written from the rule table without the engine's code, compared on 500
  generated claim-and-history worlds; a guard fails the test if the generator ever stops producing any status of any rule. The first
  version of the generator never produced a fail for E002 and the guard caught it.
- **Scenario tests** for every rule: pass, fail, not applicable, unable, and the boundaries (exactly at a limit and one over, same date
  versus the next day, equal submission dates, the same claim id, a later claim, a different provider).
- **Fuzzing** (`tests/test_fuzz_ext.py`, also in `scripts/fuzz_campaign.py`): arbitrary JSON as a claim and as a history, hostile
  text in notes, codes and modifiers, a misbehaving history object, large claims. This found one real defect: a history store that
  returned an empty dict was read as "no earlier claims"; it now fails closed.
- **Mutation checks**: 21 deliberate breakages (an ignored modifier, a boundary changed from `>` to `>=`, the provider ignored, own units
  forgotten, a raw authorization id placed in a fact line, unable ranked above fail, and others); 20 were caught by a test and the
  21st is an equivalent mutant (equal order keys imply the same claim id, which is already excluded).
- The evidence run also found that the first oracle only accepted whole-number quantities while the public data has 1.5; the oracle
  now compares exact fractions.

## Limits

The pair table, route modifiers, event-date wording and which diagnoses play which role are our choices and are open to the mentor.
Two rules (E004, E102) rest on the research report rather than a source we could confirm. Advisory findings are not yet shown in the
reviewer API or used by the routing score; that belongs to the queue work.
