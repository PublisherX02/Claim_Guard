# 61. History checks (E104 to E106) and the patient history view

A claim that is clean on its own can still be a repeat. The fifteen official rules look at one claim; the extension rules E101 to E103
(docs/30) already compare a claim with the patient's earlier claims. This document adds three more cross-claim checks and a way for the
reviewer to see the history behind a flag.

## How the history is read

When a claim is processed the server asks the store once for the patient's earlier claims (`claims_for_patient`). With encryption on, the
lookup uses the keyed hash of the patient id, so it is one indexed read and no patient identifier is searched in clear. The answer is cached
for all history rules of that claim, so adding rules adds no reads. "Earlier" means strictly before by (submission date, claim id); a claim
is never compared with itself and an earlier claim is never flagged by a later one.

The rules receive a minimal projection of each earlier claim: ids, provider, submission date, **invoice number**, lines, authorizations,
notes, diagnosis code, attachments. The invoice number was added for E104. It is a billing reference, not patient data.

## The three new checks (advisory)

They are advisory like E101 to E103: shown to the reviewer, never part of the official R001 to R015 verdict, and a missing history gives
UNABLE_TO_ASSESS, never PASS. They live in `rules/extensions/history_rules.json`, apart from `catalogue.json`, so the pack pinned for E001 to
E103 (and the Python build that shares it) is unchanged. The values (the window and the limit of E105) are fictional teaching values.

| Rule | Severity | Flags |
|---|---|---|
| E104 Invoice number reused | high | An earlier claim of the same patient and provider carries the same invoice number (case and spaces ignored). Catches a resubmission under a new claim id, including one with changed amounts. |
| E105 Many claims in a short time | medium | The patient has more than 3 claims in 7 days, this one included (configurable in the same file). |
| E106 Same service, same day, another provider | medium | A line repeats a service code and service date of an earlier claim billed by a **different** provider. E101 covers the same provider. |

**What they cannot do.** The history is per patient. A provider who reuses one invoice number for *different* patients is not caught by
E104; that needs a lookup by provider and invoice number, which is a store change. Nothing here learns from data: every flag names the
earlier fact it rests on, and a reviewer decides.

## What a flag does to routing

An advisory flag does **not** change the lane or the score: both come from the fifteen official rules only. A repeat of an earlier claim
that is otherwise clean therefore stays green (we measured this: a resubmitted claim with in-range dates is lane green, score 0). Two things
protect it:

- the reviewer sees a notice above the claim naming the failed checks, the advisory section opens by itself, and the history view is one
  click away;
- the system never decides such a claim. With `auto_clear_green` on, a green claim whose advisory check failed is not auto-cleared and not
  left to the audit draw: it always goes to a person (`flow.rs`, `advisory_failed`; test
  `a_green_claim_that_an_advisory_check_flagged_is_never_cleared_by_the_system`).

Not done: letting an advisory flag raise the score or move the claim to lane A. That changes the triage the official tests pin, and inside one
batch the earlier claims are not stored yet when the green set is drawn, so it needs the advisory run to move earlier. It is the next step.

## The patient history view

On a claim a reviewer or senior holds, the card **Patient history** has a button, *Show this patient's other claims*. It lists the other
claims of the patient (claim id, submission date, provider, total, state, which rules were flagged), newest first, at most 50. It is a
button rather than a default so that reading other claims is a deliberate act:

- only the person who holds the claim can ask (the same rule as a correction), and only a role that decides claims;
- every use is written to the security log as `patient_history_viewed` (badge, claim, number of rows);
- a row carries no patient identifier, no notes and no line detail.

Administrators cannot read claims and therefore have no such view. When an advisory check fails, the claim page shows a notice above the
claim ("Compared with this patient's earlier claims: ..."), and the advisory section opens by itself.

## Why not a trained model

There is no labelled fraud data in our claim format, so a model could not be measured honestly here, and it could not point to the fact a
reviewer needs. Public fraud datasets use other fields. The rules give the same first catch (repeats and overlaps) with an explanation. A model
could later be added as one more advisory score on top of the same history read.

## Tests

- `extensions.rs`: each rule fails and passes on constructed histories, with the boundary cases (exactly three claims in the window, seven and
  eight days back, other provider, other date, same provider), and a missing history is UNABLE_TO_ASSESS.
- The pinned pack for E001 to E103 is still the committed `rules/extensions.yar` with the Python hash.
- `service_flow.rs`: a copy of a claim carries E101 and E104; the history view returns the other claim, leaks no identifier, is logged, and is
  refused for another reviewer and for an administrator.
