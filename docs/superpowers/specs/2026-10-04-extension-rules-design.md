# Phase 2, sub-project 4: extension rules (advisory)

Shows that the team did not take the mentor's 15 rules as given: it adds eight further deterministic checks drawn from real
claim-validation practice (research report received 2026-10-04: CMS NCCI/MUE, WEDI SNIP, NPHIES, eClaimLink, and cross-claim fraud
patterns), implemented, tested and documented to the same standard as the original fifteen. Serves the rubric line "Detection
quality and benchmark performance" through initiative and rigour, without risking the score on the official rules.

## Goal
A second, separate rule pack (`E001`..`E005` on a single claim, `E101`..`E103` using earlier claims) that produces **advisory
findings** in its own output file. Reviewers can see them; they never change the official 15-rule verdict, the official
evaluation, or any frozen output.

## Non-goals
Changing, renaming or re-scoring any of R001..R015; writing extension results into the official results files or the official
schema; clinical judgment (medical necessity, drug interactions, age/sex checks, provider specialty); rules the report marked
low-value (APCI cap override, split-billing across days, impossible hours, surgical multiple-operation pricing, the report's
R016 which repeats our R013); a MongoDB history store and queue routing of advisory findings (the queue sub-project plugs into
the history interface defined here).

## Hard separation guarantees (each pinned by a test)
1. Ids are `E`-prefixed. The mentor's hidden rules are probably `R016` and up; no id of ours may collide with them.
2. The official engine files (`src/yara_engine.py`, `src/facts_extractor.py`, `rules/core.yar`, `src/evaluate.py`,
   `schemas/result.schema.json`, `rules/rules.json`, `rules/services.json`, `rules/policies.json`, `rules/diagnoses.json`) are
   byte-identical after this work; a test compares their SHA-256 with the values recorded before it started, and the official
   results on all three public splits are byte-identical to the frozen outputs.
3. Extension findings use their own schema (`schemas/extension_result.schema.json`: same fields plus `rule_family:
   "extension"`, `advisory: true`, `pack_hash`) and their own file; the official scorer rejects them by construction.
4. A failure inside one extension rule yields `UNABLE_TO_ASSESS` for that rule only (never `PASS`), exactly as in the official
   engine, and never affects the official results.

## Where the data comes from, honestly
The fictional catalogue has six services and six diagnoses, with no code-pair tables, drug flags or accident flags. The rule
*logic* below is real and cited; the *attributes it needs* are invented and live in `rules/extensions/catalogue.json`, never in
the official rule files. Every attribute is labelled fictional in that file and in the documentation.

**Measured on the 600 public claims (2026-10-04):** all 15 pairs of the six real services occur together on one date in 36 to 54
claims each; 170 claims carry a `SVC-PHARM` line and none has a route modifier; all six diagnoses are used by 90 to 109 claims;
every one of the 600 patients has exactly one claim. Any pair, route or accident rule defined on the real codes would therefore
flag dozens of claims the organizers consider valid, and the cross-claim rules cannot fire on public data at all. Decision: the
single-claim rules (E001 to E005) are defined only on **fictional codes outside the six-code teaching catalogue**
(`SVC-EXT-PRIMARY`, `SVC-EXT-COMPONENT`, `SVC-EXT-NEVER`, `SVC-EXT-RX`, `DX-EXT-ACCIDENT`, `DX-EXT-SECONDARY`). They show what a
larger catalogue would need and are verified on constructed scenarios; on public data they fire on 0 claims by design, and the
official R011 independently flags those codes as outside the official catalogue, which is correct. The history rules (E101 to
E103) use the real codes and the official limits, and are verified on constructed multi-claim scenarios, because the public set has
no patient with two claims. All of this is stated in the documentation rather than hidden.

## The eight rules
Status semantics follow the official engine: a missing input gives `UNABLE_TO_ASSESS`, never `PASS`; a rule whose trigger is
absent gives `NOT_APPLICABLE`.

| Id | Name | Logic | Source (primary, to be verified before citing) | Severity |
|---|---|---|---|---|
| E001 | Procedure pair without exception | Two lines on the same date whose service codes form a listed pair, and the secondary line carries no exception modifier (`EDU-SEPARATE`) | CMS NCCI Policy Manual ch. 1: column one / column two pairs; confirmed | medium |
| E002 | Exception modifier on a never-bundle pair | A listed pair marked `modifier_allowed: false` appears with an exception modifier anyway | CMS NCCI modifier indicator 0 (no modifier may bypass) versus 1 (may bypass); confirmed | high |
| E003 | Situational date dependency | A diagnosis marked `requires_event_date` needs an event date in the notes or attachment text, written `Event date: YYYY-MM-DD`, valid and not after the earliest service date | WEDI SNIP level 4 "if A then B" inter-segment rule; confirmed as a pattern. The diagnosis-to-event-date pairing is our fictional instance, not a published edit | medium |
| E004 | Route of administration | A service in category `pharmaceutical` (SVC-PHARM) needs a line modifier from the route list (`EDU-ROUTE-ORAL`, `EDU-ROUTE-IV`, `EDU-ROUTE-TOPICAL`) | eClaimLink publishes a Route Of Administration coding set; the rule "a drug line must carry a route" is the research report's claim and **not independently confirmed**, so the card says so | medium |
| E005 | Primary diagnosis validity | The header diagnosis is marked `secondary_only` and cannot be the primary reason for the claim | ICD-10-CM Official Guidelines: manifestation ("code first") codes cannot be first-listed; confirmed through payer policies that quote them | medium |
| E101 | Duplicate of an earlier claim | A line equals a line of an earlier claim for the same patient and provider on service code, service date, quantity and net amount | CARC 18 "exact duplicate claim/service"; confirmed | high |
| E102 | Authorization exhausted across claims | Units under one authorization id in earlier claims plus this claim exceed that authorization's `max_quantity`, when earlier claims contributed units | Cumulative authorization tracking as in NPHIES (not independently confirmed); extends our R009 | high |
| E103 | Daily quantity across claims | Units of one service on one date for one patient **and the same provider** in earlier claims plus this claim exceed the service's `max_quantity`, when earlier claims contributed units | CMS MUE date-of-service edits (MAI 2 and 3): units summed per beneficiary, provider and date; confirmed | medium |

E102 and E103 deliberately fire only when earlier claims contributed units, so they never overlap R009 (authorization within one
claim), R006 or R013, and take no side on the `EDU-SEPARATE` policy question already raised for the mentor. (Ruling, 2026-10-04: the
first public-data experiment showed E102 flagging 8 claims that R009 already fails, because it also counted a single claim's own
excess; it was narrowed to the cross-claim case.)

**Earlier claim** means: same `patient_id`, a different `claim_id`, and ordered before this one by `(submission_date, claim_id)`.
That total order makes the result for a given claim independent of how many later claims exist, and a batch run reproducible.
A corrected re-submission with the same `claim_id` is never its own history. If no history is supplied, E101..E103 give
`UNABLE_TO_ASSESS` with the message "claim history not available".

## Components
- `rules/extensions/catalogue.json`: `pairs` (primary, secondary, `modifier_allowed`), `exception_modifiers`, `route_modifiers`,
  diagnosis flags (`requires_event_date`, `secondary_only`), service `category`. All labelled fictional.
- `rules/extensions.yar` and `src/extension_rules.py`: the same facts-blob design as the official engine (details function per rule
  emits fact lines and the result fields from one pass; the YARA-X pack matches the blob), so extension rules are checked by
  the same mechanism and carry their own `pack_hash` and `engine_code_hash`.
- `src/claim_history.py`: `HistoryView` interface (`earlier_claims(claim) -> list of claims`) and `InMemoryHistory(claims)`.
- `scripts/run_extensions.py`: `--claims`, optional `--history` (default: the same file), `--out`; deterministic output.
- `schemas/extension_result.schema.json`, `docs/30_Extension_Rules.md` (a card per rule: logic, source, catalogue attributes
  invented, false-positive risks, examples), README section, SPECS section 10e.

## Testing
- **Independent oracle** `tests/oracle_extensions.py`: a second implementation written from the rule table above without reusing the
  extractor, compared on every scenario and on Hypothesis-generated claims and histories.
- **Scenario sets with expected statuses fixed by construction** (one pass, one fail, one not-applicable and one
  unable-to-assess case per rule, plus boundary cases: exactly at a limit, same date versus next day, same claim id, order ties),
  reported as tier C (hand-derived) per the existing evaluation report convention, with Clopper-Pearson bounds.
- **Separation tests** for the four guarantees, including the file-hash and frozen-output comparisons.
- **Fuzzing** in the existing Hypothesis style: malformed lines, missing fields, hostile strings in notes (fact-blob forgery:
  a note that imitates `E001:...` must not create a finding, as the official `_q` encoding already ensures), huge histories.
- **Mutation checks** of the new tests (e.g. drop the exception-modifier test, count the claim's own units, ignore order) as before.
- **Experiment** `scripts/extension_experiments.py`: fire counts of each rule on the 600 public claims (using the other public
  claims as history), runtime per claim with and without 10,000 history claims, and agreement between rules and oracle; evidence in
  `outputs/evaluation/extensions.json` with the producing commit and file hashes.

## Open items
Source check done 2026-10-04 (results in the table above). Still unconfirmed: the E004 route rule and NPHIES cumulative authorization
tracking; their cards say so. Exact page citations from the CMS manual are added to `docs/30` when the cards are written. Pair table contents and the route/exception modifier names are our choices and are
open to the mentor's view.

## Why this order
Built before the queue because the history interface is small, independent, and gives the queue sub-project something real to
route and a database-backed history to plug into.
