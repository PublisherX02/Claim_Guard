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
the official rule files. Every attribute is labelled fictional in that file and in the documentation. Because 328 of the 600
public claims put several services on one date, the pair table is chosen so that it **flags no valid public claim**; the
experiment reports how many public claims each extension rule fires on, and the number is published whatever it is.

## The eight rules
Status semantics follow the official engine: a missing input gives `UNABLE_TO_ASSESS`, never `PASS`; a rule whose trigger is
absent gives `NOT_APPLICABLE`.

| Id | Name | Logic | Source (primary, to be verified before citing) | Severity |
|---|---|---|---|---|
| E001 | Procedure pair without exception | Two lines on the same date whose service codes form a listed pair, and the secondary line carries no exception modifier (`EDU-SEPARATE`) | CMS NCCI Policy Manual ch. 1 (procedure-to-procedure edits) | medium |
| E002 | Exception modifier on a never-bundle pair | A listed pair marked `modifier_allowed: false` appears with an exception modifier anyway | CMS NCCI (correct-coding modifier indicator 0) | high |
| E003 | Situational date dependency | A diagnosis marked `requires_event_date` needs an event date in the notes or attachment text, written `Event date: YYYY-MM-DD`, valid and not after the earliest service date | WEDI SNIP level 4 (situational requirements) | medium |
| E004 | Route of administration | A service in category `pharmaceutical` (SVC-PHARM) needs a line modifier from the route list (`EDU-ROUTE-ORAL`, `EDU-ROUTE-IV`, `EDU-ROUTE-TOPICAL`) | eClaimLink / DHA data dictionary (drug code with route) | medium |
| E005 | Primary diagnosis validity | The header diagnosis is marked `secondary_only` and cannot be the primary reason for the claim | Industry diagnosis-sequencing rules (to be sourced) | medium |
| E101 | Duplicate of an earlier claim | A line equals a line of an earlier claim for the same patient and provider on service code, service date, quantity and net amount | Payer duplicate-claim edits (CARC 18 behaviour) | high |
| E102 | Authorization exhausted across claims | Units under one authorization id in earlier claims plus this claim exceed that authorization's `max_quantity` | NPHIES authorization tracking; extends our R009 | high |
| E103 | Daily quantity across claims | Units of one service on one date for one patient in earlier claims plus this claim exceed the service's `max_quantity`, when earlier claims contributed units | CMS Medically Unlikely Edits, date-of-service edits | medium |

E103 deliberately counts only units from earlier claims, so it never overlaps R006/R013 (within one claim) and takes no side on
the `EDU-SEPARATE` policy question already raised for the mentor.

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
Primary sources for E005 (sequencing) and exact wording from the CMS, WEDI, NPHIES and eClaimLink documents must be fetched and
confirmed before they are cited; the report is a secondary compilation. If a source does not support the rule as described, the
rule's card says so or the rule is dropped. Pair table contents and the route/exception modifier names are our choices and are
open to the mentor's view.

## Why this order
Built before the queue because the history interface is small, independent, and gives the queue sub-project something real to
route and a database-backed history to plug into.
