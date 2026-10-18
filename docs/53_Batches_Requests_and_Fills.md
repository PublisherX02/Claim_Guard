# 53 | Batches with an audit sample and quarantine, requests for information, and the copilot's fills

Built after the mentor session of 2026-10-18. What the mentor answered, what was built from it, what was measured, and what is still not true.

**Read this first.** Every number below comes from the organizers' synthetic claims, simulated or scripted reviewers, and a local demo server. No real officer, hospital or payer has used any of it. Nothing is delivered to a provider or a patient: the system has no outgoing channel.

## 1. What the mentor said

1. **Information that is missing and must come from outside our system** (the mentor's words, as relayed: back to the patient): the system flags it, writes the whole request itself, and the officer only confirms the last step, which is sending. **What was built goes to the provider named on the claim, not to the patient:** the claim carries no patient contact details and the patient is a pseudonym everywhere in the system. Whether the mentor meant the patient literally is an open question for him.
2. **Corrections**: the copilot fills the values in automatically, for every rule it can, and the officer confirms the fills.
3. **Green claims in a batch** (100 or 1,000): about 5% of them go to officers. When one of those is found wrong, the whole batch is quarantined at once. The rest of the 5% is still analysed by the officers. The administrator sees only the batch number, how many of the sampled green claims were flagged, and the general reason, and decides: let it go, have some of the batch's claims checked by officers, or have all of them checked.

## 2. Batches

### 2.1 What a batch is

One submission: a file given to `cg queue submit --claims` (cut into batches of at most 1,000 claims), or one request to the submit route (1 to 1,000 claims; the route takes a body of up to 4 MB, every other route 64 KB). The console's **Submit claims** page sends everything you give it in one request, so one paste or one file is one batch; above 1,000 claims it asks you to split the file. (Until 2026-10-18 the console cut a submission into pieces of 20, which made a 25-claim paste two batches with two audit samples; found in the browser test, fixed, and covered by a test.) A reverse proxy in front must let a 4 MB body through on that one path (docs/54). Every new claim carries a `batch_id`; claims that were already stored are not part of a new batch. Corrected claims (a recheck) are not part of any batch: an officer handles them.

The first batch of every demo server is the sample claims it starts with. `CG_DEMO_CLAIMS=0` starts the demo empty.

### 2.2 The audit sample is exact

The old audit drew a random number per claim (`audit_percent`, default 5). In a batch of 100 green claims that can sample none, and then nothing can ever be found. A batch now draws **exactly `max(1, ceil(percent x green / 100))` of its green claims**, without repeats, from a seed written on the batch (`batch::choose`, a ChaCha8 stream, so the draw can be re-run). 100 green claims give 5, 1,000 give 50, 19 give 1. Claims on their own (no batch) are still drawn by chance, as before.

The draw is made once, when the batch comes in, over the claims that are green at that moment (clean and intact results, the same test as auto-clear). The sampled ones are marked (`audit_sample`) and go to officers; the others are cleared by `system:auto-clear`, as before. This only matters when `auto_clear_green` is on; with it off every green claim goes to an officer, and the sample is just a record.

### 2.3 Quarantine is a status of the batch, not a rewrite of its claims

The system's clearance of up to 1,000 claims is not undone claim by claim. A batch has no mutable record; its status is the **fold of its append-only events** (`batch.rs`): `created`, `sample_verified`, `flagged`, `decision`. Statuses: `open` (nothing wrong found), `quarantined`, `in_review` (claims sent back), `review_done` (all sent back were looked at), `released`.

A cleared claim keeps its own record. Whether its clearance stands is read from its batch: `GET /api/v1/ops/trace/{claim}` shows `batch: {batch_id, status, held}`. Who reads `auto_cleared` today, and what they do with a held batch:

| Reader | Behaviour |
|---|---|
| Claim trace (`trace`) | shows the batch status and `held` |
| Shadow, feedback, workload and audit reports (`auto_cleared` counts) | unchanged: they count what the system decided, not what is currently held |
| Dashboard, exports | **not changed**: no export of "cleared" claims exists in the Rust server; a future one must check `held` |

### 2.4 The flag

`escalate` on a green claim of a batch (the officer's "the engine missed something") now **requires a category**: the rule that should have flagged it (R001 to R015) or `other`, plus an optional note of 200 characters. The note is checked against the claim's patient and member identifiers when it is written (a note containing one is refused, as for a decision reason) and is shown to the administrator, without the claim and without the officer. The check finds identifiers, not a name typed in free text. The flag is written to the batch **before** the claim moves, so a failure after it holds the batch (the safe direction). The first flag writes `batch_quarantined` to the security log; later flags only add to the counts. A flag after a release holds the batch again.

### 2.5 The administrator

`GET /api/v1/queue/batches` and `/{id}` (permission `routing.manage`) return: batch id, time, claims, clean claims, audit percent, how many were sampled, flagged, confirmed and are still waiting, the categories, the status, what was sent back. **No claim id, no claim value, no officer badge.** The officers' notes are listed (one per flagged claim). The console page **Batches** lists them and opens a decision form.

`POST /api/v1/queue/batches/{id}/decision`:
- `release`: the batch is let go.
- `review_n` with `n`: `n` of the claims the system cleared in this batch, chosen at random, go back to the officers' pool (`decided -> ready`, no longer `auto_cleared`).
- `review_all`: all of them.

Only a held batch can be decided; while claims already sent back are still with officers, a new decision is refused (409). When they are all decided the status is `review_done` and the administrator decides again. The word is "review", not "recheck": recheck already means a corrected new claim version (docs/51). Each decision is a `batch_decision` event in the security log (batch, action, how many, who).

### 2.6 Measured

- `cargo test -p cg-queue --test service_flow batch`: the sample is exact for 1, 19 and 100 green claims; one flag holds the batch, a second is not a second quarantine; a refused flag changes nothing; the administrator's view lists the note but contains neither a claim id nor the officer; a note with the patient identifier is refused; a flagged claim that a second officer then confirms is not also counted as confirmed; an officer is refused; review N, release refused while claims are out, review all; release; a later flag holds the batch again; a claim outside any batch behaves as before.
- `batch::tests`: exact size and no repeats for 1 to 1,000 green claims, same seed same draw, every position can be drawn, the fold.
- The store contract (`contract::run`) covers the new event methods in the in-memory store and **in MongoDB** (run against the `cg-test-mongo` container).
- `python rust/tools/batch_e2e.py --cg <cg.exe>` (real HTTP, a demo server it starts): 17 of 17 checks, output `outputs/defense/batch_e2e.json`.
- **Browser test, 2026-10-18** (real Chrome, Rust build, demo server): upload of 20 claims on Submit gives one batch with 1 sampled claim; flagging with no rule is refused in the page; flagging with R007 and a note holds the batch; the Batches page shows 1 of 1 checked, 1 flagged, R007, the note and no claim id; "send 5 back" works. The drafted request card and its Confirm button, and **Fill in all** with the what-if (R007 FAIL to PASS, reason pre-written), also worked.
- **A batch of 1,000** (`batch_e2e.py`): 1,000 distinct claims in one request, one batch, 1,000 clean, exactly 50 sampled. It takes tens of seconds on a debug build (about 22 s here); a proxy in front needs a read timeout above that.

## 3. Requests for information

For every flagged finding whose suggestion is "ask for it" (a missing value, an authorization, a document, an identifier mismatch), the system drafts one message per claim (`copilot::info_request`): to the provider, subject, and a numbered list, each item in the rulebook's own words (the missing fields, or the rule's corrective action), ending with "This is a request for information only. It is not a decision on the claim and not a promise of payment." It names the claim, not the patient, invents no deadline, and cannot be edited. It has a `request_id` (a hash of the claim, version and text).

The officer's whole part is **Confirm: send this request** (or **Do not send**). Confirming:
1. checks the officer may decide every finding the request covers (a request covering a high-severity finding needs a senior; otherwise nothing is sent);
2. records `request_information` on each covered finding through the ordinary decision path (the system fills in the reason), skipping findings the automatic mode already requested;
3. writes the complete message into the **outbox record** (`request_events`, kind `sent`) and `info_request_sent` to the security log.

A stale request id, a second confirmation and a confirmation after a decline are refused. `rust/crates/cg-queue/tests/service_flow.rs` covers send, decline, stale id, and the senior rule.

**What this does not do.** Nothing is delivered. "Sent" means "confirmed and placed in the outbox"; an email, SMS or letter step would read that record and does not exist. The claim stays with the officer until the information comes back, as when an officer records a request by hand. **The automatic mode (`auto_request_info`, docs/50) still records requests with no message and no confirmation, and the claim never reaches an officer; with it on, no person confirms anything.** Closing that gap needs a "requests to confirm" list for claims the system requested on its own; it is not built.

## 4. The copilot's fills, every rule

Each flagged finding gets exactly one of three routes. A value is filled in only when it follows from other values **on the same claim** (or the policy currency); anything else is asked for or checked, never invented.

| Route | Rules | What is filled, from what |
|---|---|---|
| **Fill** | R001 (a missing quantity, unit price or line amount, from the other two), R007 (line amount = quantity x price), R008 (a missing authorization reference: the one approved authorization the claim itself carries for this patient, service and date), R009 (a wrong reference: the only authorization on the claim that fits), R012 (total = sum of lines, chained with R007), R015 (the policy's currency) | the officer confirms (see below) |
| **Draft a request** | every UNABLE_TO_ASSESS, R001 fields that cannot be derived, R004 identifiers, R008/R009 when no single authorization fits, R010 documents | section 3 |
| **Check against the source** | R002, R003, R005, R006, R011, R013, R014 | a text that says what to verify; no value is proposed because none follows from the claim |

The officer's confirmation of fills: the copilot panel has **Fill in all N proposed values for me**; the system puts every proposed value into the correction form, runs the what-if, and pre-writes the reason. Nothing is saved until the officer ticks that the values were checked against the source and confirms; the result is a new claim version and the original stays as received (docs/51).

### Measured (`cargo test -p cg-queue --test copilot_eval` and `--test copilot_fills`)

On the 600 public claims, 777 flagged findings, every proposal applied to a copy and the real engine run again (`outputs/defense/copilot_eval.json`):

| Rule | Proposed | Cleared the finding | Created a new finding |
|---|---|---|---|
| R001 | 15 | 15 | 0 |
| R007 | 30 | 30 | 0 |
| R012 | 34 | 34 | 0 |
| R015 | 29 | 29 | 0 |

Applying all proposals of a claim together: 103 claims had a proposal, 68 had no flagged finding afterwards, **0 flagged a new rule**.

R008 and R009 have no case in the public claims (their failures never carry a matching authorization), so they were measured by damaging clean public claims (`outputs/defense/copilot_fills.json`): 339 lines each, the engine flagged every one, the copilot proposed the original authorization every time it was the only one that fits, applying it cleared the rule 339 of 339 times, **0 new findings**. When two authorizations fit, it holds back: a separate test duplicates the fitting authorization on a claim and checks that no value is proposed and the finding becomes a request (`with_two_authorizations_that_fit_the_copilot_proposes_nothing_and_asks`). This shows the fill is right when the claim carries the answer; it says nothing about how often real submissions do.

Only 15 of the 61 R001 failures could be filled: the others miss a header field or a value that appears nowhere else on the claim. The 61 include 46 that are asked for instead.

## 5. Limits, plainly

- Nothing leaves the system (section 3). The outbox is a record, not a delivery.
- The automatic request mode has no confirmation step (section 3).
- A batch is whatever one submission was. A batch cannot be assembled from several requests; a 1,000-claim batch needs the command line.
- The administrator's decision needs no second person. Releasing a batch the officers flagged is one click.
- The sample catches a miss in proportion to its size: with 5% of 100 green claims (5 claims) a rate of misses below about 1 in 5 can pass unnoticed; the table in docs/50 (section 3.2) says how large an audit must be to bound a miss rate. A flag is a person's opinion; this system cannot tell a real miss from a mistaken flag.
- The notes are free text by people: they are checked for the claim's identifiers, not for names or other personal details typed in.
- Quarantine is a status, not a lock: a consumer that reads `auto_cleared` without the batch (section 2.3) will treat a held claim as cleared.
- The fills are checked against the engine's own answer: a fill that clears a rule is "consistent with the rulebook", not "correct against the real bill". A person checks each against the source.
- Not driven in a browser: Release, "Send all back", and Decline on a request (the scripted run covers them).
- Auto-clear and the audit percent are now settings in the console (**Routing settings**, "Clean claims and the audit sample"); before 2026-10-18 they could be changed only through the API.


## 7. Asynchronous intake (2026-10-18)

`POST /api/v1/queue/submit` with `"async": true` answers **202** at once with `{"job_id", "state": "running", "total"}` and takes the claims in on a background thread. `GET /api/v1/queue/submit/{job_id}` (level 4) returns `state` (running, done, failed), `phase` (reading: each claim read, checked and run through the rules; storing), `done`, `total`, `seconds`, and, when done, `batch_id` and the same per-claim `results` as the synchronous call. The batch is the same batch: one id, the exact 5% audit sample, quarantine, the same refusals. The console uses this and shows "Checking 620 of 1,000 claims".

Measured (`rust/tools/batch_e2e.py`, release build, 1,000 claims, all checks pass): the request is answered in 0.06 s; the batch finishes in about 17.5 s; polling shows progress; a level 3 user cannot read the job (403); an unknown job is 404.

Limits, stated plainly:
- Jobs live **in memory**. A restart forgets them. If the server stops mid-batch, some claims are stored and the batch record may promise more than arrived; sending the batch again is the repair (a stored claim is reported `already_stored`, never stored twice), and the new submission becomes a batch of the claims that were missing.
- At most 2 batches run at once (a third gets 409 `busy`); the last 20 finished jobs are kept.
- The synchronous call (no `async`) is unchanged, for the command line and existing clients.
- Not tested: two API copies behind a load balancer (the job lives on the copy that took the request, so polling needs the same copy: sticky routing, or a shared job store, would be needed; the demo and the single-machine install are unaffected).
