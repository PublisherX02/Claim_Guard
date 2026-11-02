# 68. "Ask the rulebook" robustness, operator schedules and claim difficulty bands

## 1. Ask the rulebook: questions that are not worded like the rulebook

**Problem.** The question search worked when a question used the rulebook's own words. A loosely worded, misspelled, French or Arabic question often found nothing, and a question the rulebook cannot answer ("what is the penalty for fraud") sometimes returned a rule that merely shared a word.

**What the code does now** (`rust/crates/cg-engine/src/payer.rs`, `ask_routed`):
1. A question that names a rule, service, provider or policy the rulebook does not have (R099, SVC-XYZ, GOLD-PLUS) is refused before any model is asked (`Graph::unknown_ids`).
2. If the keyword planner finds no query and a model is configured, the model only *rewrites* the question into plain English with the rulebook's identifiers. It may answer `OUT_OF_SCOPE`. It never writes an answer; the facts still come from the rule, policy and field files. An identifier the model invents is not trusted.
3. A judge step asks the model whether the facts found contain the answer (YES/NO). Fix in this change: the judge used to run only on the passage search tier and used the model's *rewrite* as the question, so a rewrite that drifted was judged against itself and passed. It now also checks facts from the knowledge graph when a model chose the query, and it judges against the question **as the person asked it** (only a non-ASCII question, French or Arabic, is judged in its English rewrite). A NO is refused and the passages are shown as "nearest".
4. Without a model, behaviour is the keyword search only, unchanged.

**Measurement** (`python rust/tools/ask_robustness.py --model gemma3-ask:4b`, 50 questions, local Ollama `gemma3-ask:4b`):

| Kind | Before the fix | After |
|---|---|---|
| direct (rulebook wording) | 3/4 (the miss was an over-strict test expectation, corrected) | 4/4 |
| loose | 13/14 | 13/14 |
| French | 4/4 | 4/4 |
| Arabic | 3/3 | 3/3 |
| misspelled | 3/3 | 3/3 |
| graph (how parts connect) | 3/3 | 3/3 |
| out of scope (must be refused) | 8/10 | 10/10 |
| near the rulebook but not in it (must be refused) | 3/9 | 7/9 |
| **Total** | **40/50** | **47/50** |

Without the model the same set scored 34/50 (earlier measurement).

**What is still wrong (3 of 50).**
- "do I need pre-approval for physiotherapy" is now refused although R008 answers it: the judge said NO to a correct passage (a false refusal; the safe direction).
- "which rule checks the age of the patient" is answered with R004 (member and beneficiary consistency), and "what is the maximum total of a claim" with R012 (claim total equals line amounts): related, but they do not answer the question as asked. The 4B judge accepts passages that are close in topic.
These are limits of a 4B judge, not of the facts: nothing shown is invented. A larger judge model or a stricter yes/no prompt is the next step; it was not tried here.
The set is 50 hand-written questions, one model, one run: it shows the direction, not a guaranteed rate.

## 2. Operator working hours and claim difficulty bands
Routing configuration (`cg-queue/routing_config.rs`, dispatcher, API) now supports per-operator working-hour schedules (availability follows the schedule) and difficulty bands (an operator only receives claims inside their band). The admin console shows shifts and schedules as editable bubbles (`views-admin.js`).

## 3. Password change
After a forced or voluntary password change the session is cleared and the person signs in again with the new password (a notice is shown).

## 4. Not verified
The scheduling and password screens were exercised by hand in Chrome on one run; no automated test covers the bubble editor. The pre-existing Rust test suites for `cg-queue` and `cg-api` were not re-run locally (memory) and rely on CI.
