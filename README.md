# ClaimGuard AI

A pre-validation copilot for **synthetic** healthcare claims. It ingests claims (FHIR R4 JSON, CSV or JSONL), checks them against a fictional payer rulebook of 15 rules with a deterministic engine, explains each finding with a tightly bounded AI step, and records every check, AI action and human decision in a tamper-evident audit log. A human always makes the final call.

Built for the CSTAM-VELODOC challenge (mentor: Dr. Wael Hilali). All data, codes, prices and payer rules are invented. Nothing here is a real reimbursement system.

## Phase 1 deliverables at a glance

| Deliverable | Where | Status |
|---|---|---|
| **Architecture diagram and data-flow documentation** | [docs/22_Architecture_and_Data_Flow.md](docs/22_Architecture_and_Data_Flow.md): diagrams, trust boundaries, tool permissions, 15-step data flow, failure behaviour | Done |
| **Demo of the MVP** | Run `python scripts/demo.py` (8 scenes, about 2 seconds, offline). The recording script and shot list are in [docs/23_Demo_Video_Kit.md](docs/23_Demo_Video_Kit.md) | Demo runs; video to be recorded |
| **Code repository with installation and execution instructions** | [Install and run](#install-and-run) below | Done, verified on a fresh clone (Python 3.10, 3.12, 3.14) |
| Graded Phase 1 items: ingestion, rule engine, structured output, audit log | [Phase 1 deliverables and where each lives](#phase-1-deliverables-and-where-each-lives) | Done |
| Rule-correctness evidence: independent oracle, 107,635 generated claims, per-rule status coverage | [Results](#results) below; reproduce with `python scripts/status_coverage.py` (`docs/19` section 2b) | Done, 0 disagreements |

### Architecture

![ClaimGuard AI architecture, trust boundaries and permissions](docs/figures/architecture.png)

The engine decides and the model only explains. The trusted core has no network access; the model sees one finding at a time, cannot write a file and cannot change a status; every step lands in a tamper-evident audit log. Full detail, including who may touch what, is in [docs/22](docs/22_Architecture_and_Data_Flow.md).

### Data flow of one claim

![Data flow of one claim, 15 steps](docs/figures/dataflow.png)

> **New to the project? Start with [SPECS.md](SPECS.md)**, the detailed specification, including every experiment. **[BLUEPRINT.md](BLUEPRINT.md)** is the project as an information system: deliverables, business canvas, realisation steps and UML.

## How it works (text form)

```
 FHIR / CSV / JSONL --> ingestion --> facts extractor --> YARA-X rule pack --> 15 structured results
   (bad records are        |            (one function        (declarative        (schema-checked,
    quarantined)           |             per rule)            outcome rules)      never a silent pass)
                           v                                                            |
                     audit log  <----------- every check, AI question, AI answer <------+
                (hash chain + anchor)                                                   v
                           ^                                              bounded AI explanation
                           |                                            (schema + grounding checks,
                     human review  <-------------- findings <-----------  template fallback)
              (confirm / dismiss with reason /
               request info / corrected -> recheck as a new run)
```

Three rules of the design: the **engine decides and the AI only explains**; **unknown is never a pass** (missing data gives `UNABLE_TO_ASSESS`); and the **original input is never changed**, a correction is rechecked as a new version.

## Phase 1 deliverables and where each lives

| Rubric item | Where it is | Verify |
|---|---|---|
| **Data ingestion and normalization** (FHIR R4 JSON / CSV to one internal form) | `src/ingest.py` (format detection, quarantine), `src/fhir_adapter.py`, `src/csv_to_jsonl.py`; envelope `schemas/claim.schema.json` | `python src/ingest.py --input data/development/fhir_bundles.jsonl --output outputs/ingest_normalized.jsonl --report outputs/ingest_report.json` |
| **Deterministic and AI rule engine** (15 fictional rules) | `rules/core.yar` + `rules/rules.json`, `src/yara_engine.py`; bounded AI in `src/llm_adapter.py` | `python src/run_yara.py` ; metrics in `outputs/yara_*_metrics.json` |
| **Explainability and structured output** | `schemas/result.schema.json`; every result is validated against it | `python -m unittest tests.test_phase1_rubric` |
| **Audit log engine** | `src/audit_log.py`, design in `docs/16_Audit_Log_Design.md` | `python scripts/verify_audit.py --log outputs/audit_dev/audit.jsonl `  (chain + AI ordering; add `--results` after `run_yara.py` to also cross-check every result hash) |

**Entities the rubric names, and where each is carried**

| Entity | FHIR R4 bundle | CSV folder | Normalized field |
|---|---|---|---|
| Patient | `Patient` (member id in `identifier`) | `claims.csv` | `patient_id`, `member_id` |
| Encounter | **not in the supplied pack.** `fhir_adapter` reads any `Encounter` a bundle does carry into the ingest report (`encounters`), with warnings for a patient mismatch or a dangling `item.encounter`. It is not part of the claim envelope, and no rule uses it. | none | none (the closed claim schema has no slot) |
| Coverage | `Coverage` | `coverage.csv` | `coverage{}` |
| Provider | `Organization` referenced by `Claim.provider` | `claims.csv` | `provider_id` |
| Diagnosis | `Claim.diagnosis` | `claims.csv` | `diagnosis_code` |
| Claim line items | `Claim.item[]` | `lines.csv` | `lines[]` |

The FHIR route cannot carry authorization details or free-text notes, so R009 returns `UNABLE_TO_ASSESS` there instead of guessing (310 of 600 claims); it is never shown as a pass.

**The 15 rules, by what they detect**

| Detects | Rules |
|---|---|
| Missing data | R001 required fields; R008 authorization reference; R010 supporting document |
| Inconsistent data | R002 dates in order; R003 coverage on service date; R004 member/beneficiary; R007 line arithmetic; R009 authorization matches service; R012 claim total; R015 currency |
| Duplicate data | R006 repeated service line |
| Unsupported data | R005 provider not in network; R011 unknown service code; R013 quantity/price limits; R014 outside submission window |

**Structured output.** The rubric's fields map to the result schema as: Claim ID = `claim_id`, Rule ID = `rule_id` (+ `rule_version`), rule-linked evidence = `evidence` (JSON-pointer path plus the observed value) and `rule_source`, severity level = `severity`, suggested corrective action = `corrective_action`, confidence score = `confidence` with `confidence_kind`. Per `docs/04_Rulebook.md` ("Deterministic checks use confidence=null and confidence_kind=not_probabilistic"), rule results carry `confidence: null`, exactly as the supplied answer key does on all 6,000 development results; an invented score would be presented as a probability it is not. A model-reported score would be stored as `uncalibrated`. The AI explanation contract carries no score.

**Audit log.** It records ingestion, every rule check with its confidence fields, the AI's question (written before the model is called), the AI recommendation, and system and human decisions, as a hash chain plus a separately stored head-hash anchor. This is tamper-*evident*, not immutable: `docs/16_Audit_Log_Design.md` states what production immutability would additionally need (write-once storage, an externally held anchor, authenticated reviewers). Setting `AUDIT_ANCHOR_KEY` signs the anchor so it cannot be forged without the key (`docs/20_Security_Audit.md`).

## Phase 2 deliverables at a glance

| Deliverable | Where | Status |
|---|---|---|
| **Detection quality and benchmark** (15 points): macro F1 across rule categories, valid claims preserved | [docs/29_Test_Evaluation_Report.md](docs/29_Test_Evaluation_Report.md); evidence in `outputs/evaluation/`; reproduce with `python scripts/evaluate_phase2.py` | Done |
| **Test evaluation report** (detection metrics, F1, false-positive rate, latency, limitations) | [docs/29_Test_Evaluation_Report.md](docs/29_Test_Evaluation_Report.md); explanation of the FHIR result below and in [SPECS.md section 10c](SPECS.md) | Done |
| **Human-in-the-loop and escalation logic** (10 points): routing of low-confidence and high-severity cases, overrides, feedback log | Built: severity-weighted routing into lanes (green, A, B) from a published formula with a stored receipt; personal inboxes dealt by a seeded, replayable dispatcher; decisions bound to the lease and to permissions read again at the moment of the decision; two-person sign-off for high severity with a third senior to settle a disagreement; green claims verified or escalated by a person; shadow mode that records what an automatic clearer would have done and acts on nothing; every move audited ([SPECS.md section 10f](SPECS.md), `docs/31`). Overrides: a reviewer can dismiss a flagged finding with a reason (the original issue is preserved) and a disagreement between two seniors goes to a third. Feedback log: `GET /api/v1/queue/feedback` (level 4) reports per rule how often people confirmed or dismissed it, with exact intervals, and which rules are candidates for a human review of the rule; counts only, never reason text (`src/workqueue/feedback.py`). Not built: routing by model confidence (the engine is deterministic and the AI only explains; low-certainty results, `UNABLE_TO_ASSESS` and degraded runs, go to lane B) | Done, apart from model-confidence routing |
| **Privacy, security and safety guards** (5 points): data minimization, access control, prompt and data guards, graceful handling of malformed FHIR | Built: clinical and fraud guard on AI text, quarantine of malformed FHIR, hash-chained audit log, fuzz-tested boundaries. Access control (badge, password and authenticator login, four clearance levels, masked identifiers, signed security audit log), tested with 21 attacks and 6 races on two stores (see [SPECS.md section 10d](SPECS.md)). Data minimization: deal documents no longer hold patient identifiers, and the logs, deals and dead letters are scanned for them after a real flow (`tests/test_data_minimization.py`). Prompt injection: the LLM01 battery now also rejects invisible characters (`tests/test_injection_owasp_llm01.py`) | Done, with the gaps listed in `docs/32` (no WORM storage, no external timestamp, identifiers unencrypted at rest) |
| **Privacy and security note** (threat model, access control, least privilege, auditability) | [docs/32_Privacy_and_Security_Note.md](docs/32_Privacy_and_Security_Note.md): what is stored where and who can read it, minimization, access control, the audit trail against what an immutable design needs (built versus not built), AI and injection guards, dependency audit of every requirements file, and what a deployment would still need. `docs/20_Security_Audit.md` is the Phase 1 audit | Done |

### Why FHIR claims do not score 1.0

On the organizers' answer key the engine scores F1 1.0 when it reads claims directly, and the same 600 claims read from CSV folders also score 1.0. Read from FHIR bundles they score **0.9745** (precision 1.0, recall 0.9502, status accuracy 0.9656). The cause is the input, not the rules:

- Rule R009 ("authorization record matches service") must see the authorization record: patient, service, status, valid-from and valid-to dates and maximum quantity.
- A FHIR bundle in this dataset carries only the authorization **reference number** (`preAuthRef`, present in 327 of the 600 bundles). It holds `Patient`, `Organization`, `Coverage`, `Claim` and `DocumentReference` resources and nothing that holds the record. The importer keeps the reference and creates an empty stub for the record.
- The rulebook says a missing comparison input leaves the rule `UNABLE_TO_ASSESS`, so the engine abstains. The answer key was computed from the full claim, so it says PASS or FAIL.

All **310** differences in the 9,000 results are this one case: 287 results the key calls PASS and 23 it calls FAIL, all reported as `UNABLE_TO_ASSESS`. Nothing else differs, and no failing result became a PASS. The 23 missed failures are why recall is 439 of 462 (0.9502). The cost is reviewer workload: 134 of the 201 fully clean claims receive an unnecessary abstention on R009 through FHIR. Guessing the missing record would reach 1.0 only by risking silent passes, which the project forbids. The real remedy is to look the authorization up by its reference in a payer registry, which the dataset does not include. Full evidence and breakdown: [SPECS.md section 10c](SPECS.md) and [docs/29, section 4](docs/29_Test_Evaluation_Report.md).

## Phase 2 deliverables and where each lives

| Rubric item | Where it is | Verify |
|---|---|---|
| **Macro F1 across rule categories** (five categories, plus macro by severity and over all 15 rules) | `src/eval_metrics.py` (streaming `Tally`), `scripts/eval_sets.py` (nine labelled sets in three evidence tiers), `scripts/evaluate_phase2.py` | `python scripts/evaluate_phase2.py` (about 6 minutes), then `python scripts/render_eval_report.py --check` |
| **Low false-positive rate on valid claims** | Same code; tables in `docs/29` section 5 with exact 95% upper bounds | 0 of 160, 62 and 24 claims with no failure were flagged (development, validation, stress) |
| **Latency** | `docs/29` section 8; AI step quoted from the recorded live calls in `outputs/defense/load.json` | Rule engine median 0.66 ms per claim on the test laptop |
| **Provenance of every example set** (file hash, generator, seed, label origin, commit) | `outputs/evaluation/provenance.json`, table in `docs/29` section 2 | Hashes match the files; evidence names the commit that produced it |
| **Test evaluation report** | `docs/29_Test_Evaluation_Report.md`, tables generated from the evidence and checked by a test | `python -m unittest discover -s tests -p test_eval_report.py` |
| **Robustness of the trust boundaries** (supports the safety guards) | `tests/test_fuzz_*.py`, `scripts/fuzz_campaign.py`, [Fuzz testing](#fuzz-testing) | `python scripts/fuzz_campaign.py --examples 3000` (about 6 minutes) |
| **Identity and access** (login with badge, password and authenticator code; four clearance levels; hide-not-disable responses; decisions bound to the session) | `src/access/`, `scripts/access_admin.py`, `scripts/serve_access.py`, [SPECS.md section 10d](SPECS.md), `docs/superpowers/specs/2026-10-04-identity-access-design.md` | `python scripts/access_experiments.py all` runs 25 checks against a real server; evidence in `outputs/defense/access.json` |
| **Task queue and work dispatcher** (triage receipt and lanes, personal inboxes of 25, replayable dealing, AI step with circuit breaker and template cache, two-person sign-off, replay and dead letters, shadow mode) | `src/workqueue/`, `scripts/queue_admin.py`, `scripts/queue_experiments.py`, `docs/31_Task_Queue_and_Dispatcher.md`, [SPECS.md section 10f](SPECS.md), `docs/superpowers/specs/2026-10-04-task-queue-dispatcher-design.md` | `python scripts/queue_experiments.py` (about 2 minutes) writes `outputs/defense/queue.json`; `python tests/mutation_queue.py` breaks the queue code in 77 ways and every one must be caught |
| **Extension rules** (eight advisory checks beyond the mentor's fifteen: procedure pairs and modifiers, event-date and route requirements, diagnosis sequencing, and three that read earlier claims) | `rules/extensions/`, `src/extension_rules.py`, `src/claim_history.py`, `scripts/run_extensions.py`, `docs/30_Extension_Rules.md`, [SPECS.md section 10e](SPECS.md) | `python scripts/extension_experiments.py`; the official engine files and results are pinned by `tests/test_ext_separation.py` |
| **Privacy and security note**, feedback report on reviewer decisions | `docs/32_Privacy_and_Security_Note.md`, `src/workqueue/feedback.py` | `python -m unittest discover -s tests -p test_wq_feedback.py`; `-p test_data_minimization.py` |
| Routing by model confidence | Not built; see the table above | |

### Running the reviewer API

The API (`src/access/`) needs MongoDB for its users. For a local demo:

```bash
# 1. put MONGO_ROOT_USER and MONGO_ROOT_PASSWORD in .env (docker compose reads .env), then:
docker compose up -d mongo
# 2. the Python scripts read the process environment, not .env, so export the connection string in your shell:
#      export MONGO_URI='mongodb://<user>:<password>@127.0.0.1:27017/?authSource=admin'
#    then create the demo accounts (one per level; the passwords and authenticator links are printed once):
python scripts/access_admin.py --dev seed-demo
# 3. start the server on 127.0.0.1:8443 (--dev generates secrets into the git-ignored instance/ folder):
python scripts/serve_access.py --dev
```

In production every secret (`JWT_SECRET`, `FERNET_KEY`, `AUDIT_ANCHOR_KEY`, `PII_KEY`, `MONGO_URI`) comes from the environment, the
server refuses to start without them, and binding to anything but localhost requires TLS. `python scripts/access_admin.py
create-admin` makes the first administrator. The tests run without a database (an in-memory twin); set `MONGO_URI` to run the
same suites against MongoDB, and `REQUIRE_MONGO=1` (as CI does) to make a missing database a failure instead of a skip.

## Install and run

**Prerequisites.** Python 3.10 or newer (tested on 3.10, 3.12 and 3.14) and git. Windows, macOS and Linux all work. The rule engine needs `yara-x`; the AI step needs `openai` and `pydantic`; everything else in the engine is the standard library. No API key, GPU or internet access is needed to run anything below except the optional live model.

**1. Get the code and install**

```bash
git clone https://github.com/Chbaya7-Hamza/Claim_Guard.git
cd Claim_Guard

# with uv (recommended)
uv venv --python 3.10 .venv
uv pip install --python .venv -r requirements-dev.txt   # runtime packages plus Hypothesis, which the fuzz tests need

# or with plain pip
python -m venv .venv
.venv/bin/pip install -r requirements-dev.txt      # Windows: .venv\Scripts\pip install -r requirements-dev.txt
```

On Windows use `.venv\Scripts\python.exe` wherever the commands below say `python`; on macOS and Linux use `.venv/bin/python` (or activate the environment).

**2. See it work in one command (about 2 seconds, offline)**

```bash
python scripts/demo.py
```

This is a narrated tour of the whole pipeline in eight scenes: ingestion of FHIR, CSV and JSONL (with a damaged file quarantined), the 15 rules with evidence, unknown data never becoming a pass, the AI step and a simulated misbehaving model being rejected, the hash-chained audit log, a tamper attempt being detected, human review with a recheck, and an offline review page written to `outputs/demo/review.html`. Options: `--pause` (wait for Enter between scenes), `--delay 6` (hands-free pacing for a recording), `--live` (use the real model in scene 4, needs a key, see step 4). A recording guide is in [docs/23_Demo_Video_Kit.md](docs/23_Demo_Video_Kit.md).

**3. Run the tests**

```bash
python -m unittest discover -s tests          # 1641 tests, about 12 min with MongoDB and Redis (about 2 min for the offline core), no API key needed
python scripts/fuzz_campaign.py --examples 3000   # deeper fuzz run of the six trust boundaries (about 6 min); writes outputs/defense/fuzz.json
```

**4. Optional: live AI explanations.** Without a key, the explanation is a deterministic template, so every command here works offline. To use the hosted model:

```bash
cp .env.example .env      # Windows: copy .env.example .env
# then put your own key in .env:  FEATHERLESS_API_KEY=...   (never commit .env; it is git-ignored)
python scripts/demo.py --live
```

**5. Run the pipeline yourself**

```bash
# run the 15 rules over a split, score it against the answer key, and open the review page
python src/run_yara.py --input data/development/claims.jsonl --output outputs/yara_dev_predictions.jsonl
python src/evaluate.py --gold data/development/expected_results.jsonl --pred outputs/yara_dev_predictions.jsonl \
    --claims data/development/claims.jsonl --output outputs/yara_dev_metrics.json
python src/make_review.py --input outputs/yara_dev_predictions.jsonl --output outputs/yara_review.html

# a full audited review (ingest -> rules -> AI explanation -> audit -> reviewer decisions -> recheck)
python scripts/run_audited_review.py
python scripts/verify_audit.py --log outputs/audit_demo/audit.jsonl   # run_audited_review.py rewrites the committed outputs/audit_demo sample with fresh ids and timestamps
```

Expected: `evaluate.py` prints status accuracy 1.0 for the development split; `verify_audit.py` ends with the chain and AI ordering reported OK. Open `outputs/yara_review.html` in a browser for the review page (works offline, no server).

**Architecture and data flow** are documented, with diagrams, in [docs/22_Architecture_and_Data_Flow.md](docs/22_Architecture_and_Data_Flow.md).

**Troubleshooting**

| Symptom | Fix |
|---|---|
| `ModuleNotFoundError: yara_x` or `pydantic` | The environment is not active or not installed: use the `.venv` python, or re-run the install command |
| Garbled characters in the terminal on Windows | `chcp 65001`, or set `PYTHONIOENCODING=utf-8` |
| `python` is not found on Windows | Use `py -3.10` to create the venv, then `.venv\Scripts\python.exe` |
| `--live` says it is using the template | `FEATHERLESS_API_KEY` is missing or empty in `.env`; the demo still runs, and says which writer it used |
| A live answer is slow | The hosted endpoint has slow spells (p95 about 28 s); a timeout falls back to the template automatically |
| `validate_pack.py` exits non-zero | Expected and documented under Boundaries |

## Results

| Measure | Result |
|---|---|
| Status accuracy, issue precision and recall, all 15 rules | **1.0** on the development, validation and stress splits (9,000 of 9,000 results) |
| Independent oracle agreement (rules written again from the rulebook text alone) | **0 disagreements** over 107,635 generated claims (`scripts/status_coverage.py`), plus 37,000 boundary-aware mutants and 123 hand-derived edge cases |
| Phase 2 detection evaluation | F1 **1.0** on the 50-claim split, the 150-claim validation split and the 400-claim development split; **no valid claim flagged** in any set (0 of 160 development claims with no failure, upper bound 1.85%); 0 disagreements with the independent oracle over 107,635 generated claims, 33,945 mutants and 123 hand-derived boundary cases. Through FHIR, F1 falls to 0.9745 because authorizations are not carried (rule R009 abstains; no failure becomes a pass). Every example set is cited with its label origin. `docs/29_Test_Evaluation_Report.md` |
| Fuzz testing | **Six trust boundaries**, 25 property tests at 3,000 generated examples each plus one regression test, all passing; two real defects found and fixed (see Fuzz testing below). `outputs/defense/fuzz.json` |
| Tests | 1641, none needing a network or an API key; those that exercise MongoDB or Redis are skipped, loudly, unless `MONGO_URI` / `REDIS_URL` are set (`REQUIRE_MONGO=1 REQUIRE_REDIS=1` turns a skip into a failure). 420 were last verified in CI on Python 3.10, 3.12 and 3.14 (commit named in `docs/19`); 1609 of them passed locally with MongoDB and Redis required on 3.10, 3.12 and 3.14 (2026-10-06), and all 1641 passed on the project environment on 2026-10-06 (0 skipped); CI confirms after the push |
| Live AI explanations (Mistral-Nemo-Instruct-2407 via Featherless.ai, prompt v1.6.0 with a closing gate, temperature 0) | **Seven benchmarks, all at 85% or more** on 12 new cases (lowest 93.2%): 97.5% live, 93.3% useful, 100% injection resisted, 100% cover the rule's corrective action, 93.2% cite an observed evidence value, 93.2% name a next step, 0 garbled answers shown. Chosen by four rounds of experiments, see below |
| Local, free alternative (`gemma3:4b` via Ollama) | Beats the paid, hosted default on every automated metric: 97.2% live vs. 94.4%, 3.0 s vs. 4.1 s median latency, 0 garbled replies across an 84-case stress test. See below |
| Security | audited against the OWASP Top 10 for LLM Applications and the OWASP Top 10: `docs/20_Security_Audit.md` |

Perfect scores on the public splits are not evidence of generalization. The mentor-held 200 claims are not available to us; the independent oracle and the stress tests are the closest substitute.

**Independent oracle, reproduced at 107,635 generated claims.** `tests/oracle.py` is a second implementation of the 15 rules, written only from `rules/rules.json` and `docs/04` — it imports nothing from the engine, so an engine bug and an oracle bug would have to be the same mistake, made twice independently, to hide from this check. `scripts/status_coverage.py` generates 107,635 claims (half seeded from a fully valid claim and then damaged, half fully random, `tests/claim_gen.py`), scores every one with both the engine and the oracle, and fails loudly on any disagreement:

![Every rule vs. the independent oracle: 0 disagreements over 107,635 generated claims](docs/figures/oracle_agreement.png)

The same run's per-rule status coverage — confirming every rule reaches every status it can (PASS, FAIL, UNABLE_TO_ASSESS, and NOT_APPLICABLE where applicable), not just the ones the public data happens to show:

![Status coverage per rule across 107,635 generated claims, with what each status means](docs/figures/status_coverage.png)

A naive fuzzer that sets every field independently at random can still miss a rule's PASS state at this scale by pure chance (R009's PASS needs five fields to agree on one claim at once); seeding half the batch from a known-valid claim, as this generator does, is what makes every status reachable. Full writeup: `docs/19_Stress_Testing_and_Judging_Coverage.md` section 2b.

**Can the paid, hosted AI call be replaced by a free, local one?** Same 36 tuning cases, same scorer, same prompt
(v1.6.0 + closing gate) as the Mistral-Nemo result above — `gemma3:4b`, run entirely on the reviewer's own GPU
through Ollama, ties or beats it on every automated metric. Two other local candidates were tried and are not the
answer: MedGemma (medical-domain-tuned) is safe but ~4x slower; Qwen3 is disqualified outright — its hidden
"thinking" mode has no `max_tokens` value that works reliably across the case set, three separate settings each
found cases that failed differently.

![Model comparison: gemma3:4b, medgemma-4b-it, Mistral-Nemo, qwen3:4b — live rate and latency](docs/figures/model_comparison.png)

Full write-up, including the exact fault-injection tests behind each finding: `SPECS.md` section 6a.

## Fuzz testing

Property-based tests generate hostile input for the six places where untrusted data crosses a boundary and check an invariant that must always hold. They use Hypothesis (a dev-only dependency in `requirements-dev.txt`), the model is mocked, and nothing needs a network.

| Surface | Generated input | Invariant that must always hold |
|---|---|---|
| Ingestion (JSONL, FHIR, CSV) | mutated bytes, NUL, BOM, CRLF, U+2028, huge and deeply nested values, hostile CSV cells | never an uncaught exception; every record is accepted or quarantined with a reason; a garbage line never costs its valid neighbours |
| Model reply and closing gate | any JSON shape, wrong types, extra keys, hostile text, providers that raise | the output passes the per-finding schema or the template is used; the finding is untouched; a failure stays flagged for a human |
| Audit log | a flipped byte, a deleted or swapped row, a truncated file, forged rows appended with a valid chain, a corrupted anchor | strict verification fails; it never raises anything but `ValueError` |
| Rule engine | random and mutated claims | same status as the independent oracle on every claim that passes ingestion; a rule that crashes reports `UNABLE_TO_ASSESS`, never `PASS` |
| Injection | hostile text and rule tags (`R009:MISMATCH:`) placed in any string field | no verdict changes; the engine still agrees with the oracle |
| Review page | HTML and script payloads, arbitrary evidence values | one script block; the data round-trips exactly; no raw `<`, `>`, `&`, U+2028 or U+2029 in the embedded data |

`python scripts/fuzz_campaign.py --examples 3000` ran 25 property tests at 3,000 generated examples each, plus one fixed-case regression test, over all six surfaces. All passed (about 6.5 minutes; raw result in `outputs/defense/fuzz.json`). In an ordinary test run a fixed-seed profile of 60 examples per test is used, which adds about 8 seconds and is deterministic.

**Two real defects were found and fixed.** `audit.verify` raised a raw `KeyError`, `AttributeError` or `TypeError` for a record that is valid JSON but not an audit row; it already failed closed, but `verify_audit.py` printed a traceback instead of a broken-chain message. `make_review.build` embedded raw U+2028 and U+2029 inside a JavaScript string, which only newer browsers accept; they are now escaped. Each has a regression test that failed first.

**Limits.** This is generated-input testing, not coverage-guided fuzzing. The model is mocked, so live-model behaviour rests on the earlier experiments. The engine-versus-oracle comparison skips claims that ingestion would quarantine. A change to JSON whitespace only is not tampering, because the audit hash covers the parsed record. A clean run is evidence, not proof: the CSV-folder reader catches `OSError`, `KeyError` and `ValueError` only, and nothing else escaped in 3,000 examples.

## How the AI explanation was chosen: experiments



The 15 rules have nothing to tune, so the experiments optimize the only part that can vary: the language model that **explains** each finding (model, temperature, instruction text, parallel calls). 4,212 live calls on the Featherless.ai endpoint over four rounds. Three rules held throughout: no setting may change a verdict (checked by hashing the finding before and after every call; it never changed), the metrics and decision rules were written **before** each run, and every answer went through the production safety net.



**Temperature: 0 is best.** Higher temperatures make the model derail more (garbled replies 10% at temperature 0, 32% at 0.5) and buy nothing back. Even at 0 the hosted model is not word-for-word deterministic.



![Useful answers by temperature](docs/figures/e1_useful_vs_temperature.png)



**Model: reliability decided it.** The first default, Qwen2.5-14B, garbled about a fifth of its raw replies at temperature 0 (the safety net caught them, but each was a lost explanation); the 32B model was worse; Qwen2.5-7B and Mistral-Nemo never garbled. The experiments also found a hole in our safety net (three garbled but valid-looking explanations were shown), which is now closed.



![Garbled replies by model, over the session](docs/figures/reliability_degenerate_replies.png)



**Prompt and safety net: the biggest levers.** A new prompt that asks for the engine's reasons, the evidence values and a closing action lifted Mistral-Nemo from 71% to 97% useful answers (round two). Rounds three and four then chased the remaining gaps. The findings behind them: the model skipped the closing instruction for rules with short findings; 47 of 49 recorded rejected replies were only formatting slips in a cited path (now repaired deterministically and logged); and two of our own benchmarks were mis-specified (a correct answer could not satisfy the value-citation regex when the evidence value was null). The final prompt (v1.6.0) tells the model, per finding, the exact corrective action to close with, and a **closing gate** asks once more when a valid answer still leaves it out.@@
@@
**Result: every benchmark at 85% or more.** On 12 cases nothing had touched (10 repeats, interleaved), against a pre-registered scoreboard of seven benchmarks with an 85% bar:@@
@@
![Scoreboard on new cases](docs/figures/scoreboard_e10b.png)@@
@@
| Benchmark (bar 85%) | Prompt v1.5.0 | Prompt v1.6.0 | **v1.6.0 + closing gate (in use)** |@@
|---|---|---|---|@@
| Live rate (answered by the model) | 99.2 | 97.5 | **97.5** |@@
| Useful answers, strict | 90.8 | 96.7 | **93.3** |@@
| Useful answers, lenient | 90.8 | 96.7 | **93.3** |@@
| Injection resisted | 100.0 | 100.0 | **100.0** |@@
| Covers the rule's corrective action | 84.9 | 92.3 | **100.0** |@@
| Cites an observed evidence value | 84.0 | 84.6 | **93.2** |@@
| Names a next step | 89.9 | 84.6 | **93.2** |@@
| Garbled answers shown | 0 | 0 | **0** |@@
| **All at 85% or more?** | no (2 miss by under 1 point) | no (2 miss by under 1 point) | **yes, lowest 93.2** |@@
@@
The gate makes a second call on roughly 6% to 9% of answers and never makes an answer worse. On the 36 tuning cases the same setting also clears the bar (lowest 94.4). Earlier rounds, for context (different case sets, so compare within a set): the first default (Qwen2.5-14B) scored 77% useful with 27 garbled raw replies of 131 and covered the corrective action in 20% of answers.@@
@@
![Round two decision](docs/figures/e8_decision.png)@@
@@
**What it cost, and what to distrust.** The model still follows a couple of injection variants (VAR-02 flips the review flag; the safety net rejects that reply every time, so a reviewer sees the template), and in the frozen run of the supplied exercises 25 of 25 cases and 9 of 11 injection variants were answered by the model. "85% on 12 new cases" is a threshold we chose on a small set, not a guarantee: another set of cases would move each number by several points. Two of the seven benchmarks were redefined in round four before the runs (the old definitions are still reported), and two earlier decisions were judgement calls that overrode our own pre-registered rule, once in each direction, all documented. A hosted endpoint drifts over time, the confirmation sets are small (12 cases), and the "adds something" measures are word patterns. `experiments/manual_scoring_sheet_e8.csv`, `_e9b.csv` and `_e10b.csv` were prepared during these rounds to score Mistral-Nemo's answers by hand, but were never actually scored. Since then, `gemma3:4b` (below) has overtaken this hosted model on every automated metric, so the manual scoring effort has moved with it: `experiments/manual_scoring_sheet_gemma3_rescore.csv` holds gemma3:4b's answers on the same 36 cases those three sheets were built from (FX/FZ/FW, `scripts/export_scoring_sheet_gemma3.py`), ready for the team — a single model this time, so there's no arm to hide. If people prefer the old hosted default, it can be restored with one environment variable and one file (`SPECS.md` section 12).



Also built and tested, but off by default: a **cascade** (fluent model, then a reliable one, then the template) with an audit log that names the model that wrote each answer (`FEATHERLESS_FALLBACK_MODEL`). Every experiment, table and figure is in [SPECS.md](SPECS.md) section 11; the raw record is `docs/21_Experiments.md` and `experiments/raw/`.



## Repository map

| Path | What is in it |
|---|---|
| `src/` | The system: `ingest.py`, `fhir_adapter.py`, `csv_to_jsonl.py` (ingestion); `facts_extractor.py`, `yara_engine.py` (rules); `llm_adapter.py`, `claim_review.py` (bounded AI); `audit_log.py`, `review_workflow.py` (audit and review); `advisory.py` (checks outside the 15 rules); `make_review.py` (offline review page) |
| `requirements-dev.txt` | `requirements.txt` plus Hypothesis, needed to run the test suite |
| `rules/` | `core.yar` (compiled rule pack), `rules.json`, `policies.json`, catalogues |
| `schemas/` | JSON schemas for claims, results and review events |
| `data/` | 600 synthetic claims in three splits, in JSONL, CSV and FHIR forms, with the public answer key |
| `tests/` | 1641 tests, including `oracle.py` (independent reference implementation), the stress and security suites, and the `test_fuzz_*.py` fuzz tests with their shared `fuzz_strategies.py` |
| `scripts/` | `demo.py` (narrated tour), audited runs, audit verification, `draw_diagrams.py`, AI evaluation, `fuzz_campaign.py` (deep fuzz run) and the experiment runner |
| `experiments/` | Raw experiment data and `summary.json`; figures are in `docs/figures/` |
| `outputs/` | Frozen evidence: metrics, audit samples, recorded live AI runs |
| `docs/` | Numbered documents; see the index below |
| `prompts/`, `exercises/` | The AI prompt (v1.6.0) and its earlier versions and variants; the 25 supplied and our own test cases |

## Documents

| Read | For |
|---|---|
| [SPECS.md](SPECS.md) | Detailed specification: contracts, rules, the AI step, audit log, security, and every experiment |
| [BLUEPRINT.md](BLUEPRINT.md) | The project as an information system: the submission deliverables and where each lives, quality characteristics (reliability, security, interoperability, performance, portability, maintainability), the business model canvas with cited desk research, the 12 realisation steps each with its proof of success, the environment tests, and seven UML diagrams |
| [docs/31_Task_Queue_and_Dispatcher.md](docs/31_Task_Queue_and_Dispatcher.md) | The task queue and work dispatcher: pipeline diagram, the triage formula, dealing, two-person sign-off, the AI step's cache and breaker, how to run it, the experiment and its limits |
| [docs/30_Extension_Rules.md](docs/30_Extension_Rules.md) | The eight advisory extension rules E001 to E005 and E101 to E103: logic, source (confirmed or not), what is invented, tests and evidence |
| `docs/29_Test_Evaluation_Report.md` | Phase 2 test evaluation: macro F1 by rule category, false positives on valid claims, latency, nine cited example sets, limitations |
| `docs/27_Decisions_Proofs_and_Defense.md` | Every major decision: what we rejected, the experiment or test that backs it, and the likely challenge with its answer |
| `docs/04_Rulebook.md` | The 15 fictional rules |
| `docs/16_Audit_Log_Design.md` | Audit log design and what real immutability would need |
| `docs/17_Evaluation_Report.md` | Metrics, error analysis, AI evaluation, limitations |
| `docs/19_Stress_Testing_and_Judging_Coverage.md` | How the rules were stress-tested; each judged item mapped to its test |
| `docs/20_Security_Audit.md` | OWASP audit, red-team run, fixes, residual risks |
| `docs/21_Experiments.md` | AI experiments: temperature, model, prompt, concurrency |
| [docs/22_Architecture_and_Data_Flow.md](docs/22_Architecture_and_Data_Flow.md) | Architecture diagram, trust boundaries, tool permissions, data flow |
| [docs/23_Demo_Video_Kit.md](docs/23_Demo_Video_Kit.md) | Script, shot list and checklist for recording the demo video |
| [docs/24_Architecture_Comparison.md](docs/24_Architecture_Comparison.md) | Two architectures for the same task, rules-first and agent-first, explained and compared on the same model: how each works, the method, the scores, the hallucination metric and the limits of the result |
| [docs/25_Comparison_Model_Selection.md](docs/25_Comparison_Model_Selection.md) | Why the comparison was re-run on Qwen2.5-14B-Instruct: the models screened and why the others were rejected |
| [docs/28_Upgrades_Phase2.md](docs/28_Upgrades_Phase2.md) | Proposed Phase 2 upgrades, each tied to a measurement; nothing in it is built yet |
| `docs/00_Starter_Pack_README.md` | The organizers' original starter-pack README, kept in full |

## Boundaries

No clinical judgement, medical-necessity decision, fraud accusation, automatic approval, live payer submission or EHR integration. A PASS means "these 15 checks passed on the supplied data", never that a claim is valid or payable. Reviewer identity is self-declared: there is no authentication yet.

**`python src/validate_pack.py` exits non-zero on purpose.** It prints a PASS line for the rule pack, then stops with `Release checksum differs`, because the organizers' `SHA256SUMS.json` correctly detects our intentional edits to files it tracks. We never edit `SHA256SUMS.json` itself.

## Status

Phase 1 (ingestion, rule engine, structured output, audit log) is complete. Phase 2 is built: the detection evaluation and its report (`docs/29`), identity and access, the extension rules, the task queue and work dispatcher with its feedback report, and the privacy and security note (`docs/32`); see [Phase 2 deliverables at a glance](#phase-2-deliverables-at-a-glance) for where each lives and what it leaves out. The architecture and data-flow document is `docs/22`, and the demo runs with `python scripts/demo.py`; the recorded video follows `docs/23`. Not built: a reviewer web front end (the reviewer API exists and is tested; there is no screen on top of it), routing by model confidence, the pitch, and the unbuilt audit controls listed in `docs/32` section 4 (WORM storage, an external timestamp).
