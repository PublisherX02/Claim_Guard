# 46 | Test evaluation report for the Rust build: detection metrics, F1, false-positive rate, latency, limitations

This is the Phase 2 evaluation deliverable for the system that now runs (the Rust build). `docs/29` is the longer report on the Python reference, with nine labelled example sets and the oracle, fuzz and mutation evidence; this page re-measures the Rust engine with the **same scoring code** (`src/eval_metrics.py`) against the organizers' answer key, and adds what the Rust build changed: ingestion of FHIR and CSV, document intake, latency, and the limits. Re-run: `python rust/tools/phase2_eval.py` (writes `outputs/evaluation/rust_metrics.json`).

## 1. Detection quality (FAIL detection against the answer key)

| Split | Claims | Precision | Recall | F1 | Macro F1 over rule categories | Status accuracy | Disagreements with the key |
|---|---|---|---|---|---|---|---|
| 50-claim split (`data/stress`) | 50 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 0 |
| `data/validation` | 150 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 0 |
| `data/development` | 400 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 1.0000 | 0 |

By rule category (F1, true positives): completeness and arithmetic 1.0 (11 / 37 / 82), eligibility and coverage 1.0 (3 / 30 / 101), timing 1.0 on validation and development (11 / 31; the 50-claim split has no failing timing example, so its F1 is undefined), authorization and documentation 1.0 (6 / 14 / 41), catalogue, pricing and duplicates 1.0 (6 / 25 / 64). Per rule, every rule that has a failing example scores 1.0; **in the 50-claim split 9 of the 15 rules have no failing example, so their F1 is undefined and left out**, which is why that split is thin evidence on its own and the 150- and 400-claim splits are reported beside it.

## 2. Valid claims are preserved (false positives)

| Split | Claims the key says have no FAIL | Flagged by the engine | 95% upper bound on the true rate | Clean results wrongly flagged |
|---|---|---|---|---|
| 50-claim split | 24 | 0 | 11.7% | 0 of 180 |
| validation | 62 | 0 | 4.7% | 0 of 795 |
| development | 160 | 0 | 1.9% | 0 of 2,040 |

Upper bounds are exact (Clopper-Pearson). Zero observed is not zero true: with 160 such claims the true false-positive rate is below 1.9% with 95% confidence.

## 3. Human-in-the-loop routing and abstention

Every FAIL and every UNABLE_TO_ASSESS (the engine's low-confidence state: it cannot decide because an input is missing or unknown) carries `requires_human_review` and a corrective action; this was checked on all three splits. The confidence field is `null` with kind `not_probabilistic` because the verdicts are deterministic: nothing here pretends a rule result has a probability. Routing of what a person must look at (lanes, senior-only high-severity findings, two-person sign-off, overrides with a reason, feedback counts) is in `docs/31` and `docs/38`.

## 4. Latency

The rule engine takes **0.19 ms per claim** (the difference between a 400-claim and a 50-claim run of `cg evaluate`, so process start-up cancels out). The step that dominates end to end is the optional language-model explanation (seconds per claim, with a 90-second ceiling and a template fallback); the engine's share of the whole is under 0.03%. Reading a document is a separate, slower, optional path: about 5 s per page with the two readers on the development laptop's RTX 4060 (docs/45).

## 5. Ingestion: FHIR and CSV, and damaged input

`cg ingest` accepts normalized JSON/JSONL, FHIR bundles and the CSV tables, and quarantines any record it cannot accept with a reason, never stopping the run. `rust/tools/compare_ingest.py` (35 checks): the 600 public claims in all three formats give claims and reports identical to the Python reference; 12,000 randomly damaged bundles reach the same accept-or-quarantine verdict and, when accepted, the same claim; every accepted damaged claim gets fifteen engine results. FHIR loses what it does not carry (authorization details), so those rules abstain rather than guess (docs/29 section 4).

## 6. Limitations (read before quoting a number)

- The public pack is synthetic and small; perfect scores on it do not show the system beats an alternative, and the key is the organizers' own rulebook.
- Nine of fifteen rules have no failing example in the 50-claim split.
- The false-positive figures are upper-bounded, not zero.
- Latency was measured on one laptop, not on the target server, and throughput under load is unmeasured.
- Document reading is evaluated on 240 synthetic documents only (docs/45); real paper, handwriting and Arabic are untested.
- The advisory extension rules (E001-E103) are outside the official score.
