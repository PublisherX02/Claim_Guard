# SPECS.md: ClaimGuard AI technical specification

The detailed specification of what the system does, how each part is configured, and how the AI step was chosen and tuned. Read [README.md](README.md) first for the overview and `docs/27_Decisions_Proofs_and_Defense.md` for the reasons behind the decisions. Where a statement rests on data, the source is named; the raw audit trail of the experiments is `docs/21_Experiments.md` and `experiments/raw/`.

**Contents**

1. [Scope](#1-scope)
2. [Architecture and data flow](#2-architecture-and-data-flow)
3. [Data contracts](#3-data-contracts)
4. [Ingestion](#4-ingestion)
5. [Rule engine](#5-rule-engine)
6. [AI explanation step](#6-ai-explanation-step)
7. [Audit log](#7-audit-log)
8. [Human review workflow](#8-human-review-workflow)
9. [Security](#9-security)
10. [Verification](#10-verification)
11. [Experiments in detail](#11-experiments-in-detail)
12. [Configuration reference](#12-configuration-reference)
13. [Limits and open items](#13-limits-and-open-items)

---

## 1. Scope

**In scope.** Pre-validation of **synthetic** healthcare claims against a **fictional** payer rulebook of 15 rules; a bounded language-model explanation of each finding; a tamper-evident audit log; a human review workflow with recheck.

**Out of scope, by design.** Clinical judgement, medical-necessity decisions, fraud accusations, automatic approval, live payer submission, EHR integration, real patient data. A PASS means "these 15 checks passed on the supplied data", never that a claim is valid or payable.

**Invariants** (each enforced by tests):

| # | Invariant |
|---|---|
| I1 | The rule engine decides; the model never changes a status, rule id, evidence or review flag. |
| I2 | Unknown is never a pass: missing, unusable or unverifiable data gives `UNABLE_TO_ASSESS` (or `FAIL` when a violation is proven). |
| I3 | The original claim is never modified; a correction is rechecked as a new run. |
| I4 | Every check, AI question, AI answer, system decision and human decision is written to the audit log; the AI question is written before the model is called. |
| I5 | Every model reply is validated (schema, citations, grounding, garbled-text) before use, whatever provider produced it; failure means the template is used. |

## 2. Architecture and data flow

Diagrams, the trust-boundary table, the per-component tool-permission table and the numbered data flow are in `docs/22_Architecture_and_Data_Flow.md`; the summary below is the text form.

```
 FHIR R4 bundle | CSV folder | JSONL
            |
            v
      ingestion (src/ingest.py) ----- bad record ----> quarantine (reason + source reference)
            |  one claim envelope (schemas/claim.schema.json)
            v
   facts extractor (src/facts_extractor.py)      one function per rule -> facts, evidence paths, line ids, message
            |  facts blob (claim data percent-encoded)
            v
   YARA-X rule pack (rules/core.yar)             outcome per rule from the facts
            |  precedence: FAIL > UNABLE_TO_ASSESS > NOT_APPLICABLE > PASS
            v
   15 results per claim (schemas/result.schema.json)     ---------------------> audit log
            |                                                                     ^
            +--> FAIL / UNABLE findings --> AI explanation step ------------------+
            |                               (validated; template fallback)        |
            v                                                                     |
   review queue --> human decision (confirm / dismiss + reason / request info / corrected) --+
                         |
                         +--> corrected claim --> recheck as a NEW run
```

| Module | Responsibility |
|---|---|
| `src/ingest.py`, `fhir_adapter.py`, `csv_to_jsonl.py`, `jsonl_reader.py` | Read three input shapes, normalize, quarantine bad records |
| `src/facts_extractor.py`, `rules/core.yar`, `src/yara_engine.py` | The 15 rules |
| `src/advisory.py` | Notes for defects the 15 rules do not cover (never scored) |
| `src/llm_adapter.py`, `src/claim_review.py`, `prompts/` | The AI step and its orchestration |
| `src/audit_log.py`, `src/audit.py`, `src/review_workflow.py` | Audit log, decisions, recheck |
| `src/make_review.py` | Offline review page |
| `src/run_yara.py`, `src/evaluate.py` | Batch runner and organizer's scorer |

## 3. Data contracts

**Claim envelope** (`schemas/claim.schema.json`, closed: unknown keys are rejected). Top level: `schema_version`, `claim_id`, `invoice_number`, `patient_id`, `member_id`, `provider_id`, `payer_id`, `policy_id`, `diagnosis_code`, `submission_date`, `currency`, `total_amount`, `coverage{}`, `lines[]`, `authorizations[]`, `attachments[]`, `notes`. A line has `line_id`, `service_code`, `service_date`, `modifier`, `quantity`, `unit_price`, `net_amount`, `authorization_id`. Null means unknown; an empty array is a known-empty inventory.

**Result record** (`schemas/result.schema.json`): 15 per claim, one per rule.

| Field | Meaning |
|---|---|
| `claim_id`, `rule_id`, `rule_version` | Identity; the version belongs to the rulebook |
| `status` | `PASS`, `FAIL`, `UNABLE_TO_ASSESS`, `NOT_APPLICABLE` (`NOT_IMPLEMENTED` is a visible incomplete state, never a pass) |
| `severity` | `high` or `medium`, from the rulebook |
| `affected_line_ids` | Lines that caused the finding |
| `evidence` | List of `{path, value}`: a JSON pointer into the original claim and the exact value seen |
| `rule_source`, `explanation`, `corrective_action` | Rule reference, the engine's own sentence, and what a human should do |
| `confidence`, `confidence_kind` | `null` and `not_probabilistic` for rule results (`docs/04`); a model score would be `uncalibrated` |
| `requires_human_review` | True exactly for FAIL and UNABLE_TO_ASSESS |
| `method`, `review_status` | `deterministic`; `unreviewed` until a human acts |

**Review decision** (`schemas/review_event.schema.json`): `claim_id`, `rule_id`, `action` (`confirm_issue`, `dismiss_with_reason`, `request_information`, `mark_corrected_for_recheck`), `actor`, `reason` (required), `created_at`, `original_status` (must match the finding's real status). Extra fields are rejected.

## 4. Ingestion

| Input | Path | Notes |
|---|---|---|
| Normalized JSONL | one claim per line | BOM, CRLF and blank lines tolerated; each line decoded and parsed on its own |
| FHIR R4 bundle | `fhir_adapter.bundle_to_claim` | Reads only what the bundle carries; authorizations and notes are not carried by FHIR, so R009 is `UNABLE_TO_ASSESS` there (310 of 600 public claims) and never a pass; `Encounter` is reported, not part of the envelope |
| CSV folder | `csv_to_jsonl.convert` | `claims.csv`, `lines.csv`, `coverage.csv`, `authorizations.csv`, `attachments.csv`; a text cell in a numeric column stays text so only that claim is quarantined; orphan rows ignored; Excel BOM tolerated; non-ASCII digits are not read as numbers |

**Quarantine.** A record that cannot be read, mapped or that fails the transport contract gets a reason and a source reference and is counted; it never stops the batch and never becomes a pass. `run_yara.py` gives an identifiable but invalid claim 15 fail-closed `UNABLE_TO_ASSESS` results and exits with code 2.

**Robustness limits verified by test:** invalid UTF-8 on one line, an integer of more than 4,300 digits, 100,000-deep nesting, `null`/number/array lines, U+2028 inside values, NaN and Infinity, a 5 MB field, a 5,000-line claim.

**Dates** must be exactly `YYYY-MM-DD`. Python 3.11 and later also accept `20261231` and week dates; the rules reject them on every version so verdicts do not depend on the interpreter.

## 5. Rule engine

**Pipeline.** `facts_extractor.rXXX_details()` reads the claim and returns facts (short tagged strings), evidence paths, affected line ids and a message. The facts of all rules are joined into one blob that `rules/core.yar` matches by substring; each YARA rule carries `rule_id` and `outcome` metadata. `yara_engine.evaluate` resolves precedence and assembles the results.

**Fail closed.** A rule that raises becomes `UNABLE_TO_ASSESS` for that rule only and the error is reported; a rule with no outcome stops the run (`EngineError`: extractor and pack out of sync); wrong-typed values, missing keys and non-object rows read as unknown.

**Fact injection defence.** Claim data is percent-encoded before it enters a fact, so a value such as `USD R009:MISMATCH:` cannot forge a finding for another rule.

**The 15 rules.**

| Rule | Title | Severity | Detects |
|---|---|---|---|
| R001 | Required claim information | high | Missing data |
| R002 | Service and submission chronology | high | Inconsistent data |
| R003 | Coverage active on service date | high | Inconsistent data |
| R004 | Member and beneficiary consistency | high | Inconsistent data |
| R005 | Provider in the supplied network | high | Unsupported data |
| R006 | Possible duplicate service lines | medium | Duplicate data |
| R007 | Line arithmetic | high | Inconsistent data |
| R008 | Required authorization reference | high | Missing data |
| R009 | Authorization record matches service | high | Inconsistent data |
| R010 | Required supporting document | medium | Missing data |
| R011 | Service code in fictional catalogue | high | Unsupported data |
| R012 | Claim total equals line amounts | high | Inconsistent data |
| R013 | Quantity and price limits | medium | Unsupported data |
| R014 | Submission window | medium | Unsupported data |
| R015 | Currency matches policy | high | Inconsistent data |

**Readings adopted where the rulebook is silent** (each covered by a test):

1. A proven violation beats a missing input, in every rule.
2. "Empty" means null or a whitespace-only string (the supplied baseline's convention); identifiers and enum values compare exactly and case sensitively.
3. A quantity must be a positive whole number; `3.0` counts as whole (JSON does not distinguish it from `3`), `1.5` does not.
4. Amounts are compared exactly: `|submitted - round_half_up(expected)| <= 0.01`.
5. NaN, Infinity and wrong-typed numbers are unknown; R001 reports them as missing information.
6. R014 with any unknown service date is `UNABLE_TO_ASSESS`.
7. A service code outside the catalogue makes R008, R009 and R010 unable for that line unless another line proves a `FAIL`.

**Advisories** (`src/advisory.py`): a diagnosis code not in `rules/diagnoses.json`, and a payer that differs from the policy's. They are audit events that route a claim to a human; they are not rule results and never affect scoring.

### 5a. Why YARA-X, not plain Python: benchmarked, not asserted

The obvious question for a declarative rule engine is whether it earns its keep over just writing the 15 rules as
Python functions. We already had the fair comparison to test this against: `tests/oracle.py`, the independent
Python reimplementation used to check the engine's correctness (section 10), is a real, from-scratch, already
oracle-validated set of 15 rule functions with its own `RULES = {rule_id: function}` registry — not a strawman
written to lose. `python scripts/benchmark_yara_vs_python.py` runs the comparison below and is reproducible on
demand.

**Speed.** On 20,000 generated claims (`tests/claim_gen.py`, seed 20260927): YARA-X averages 1.752 ms/claim
(571 claims/s); the plain-Python oracle averages 0.158 ms/claim (6,346 claims/s). **The Python implementation is
about 11x faster**, and we are not going to spin that. Neither number is the reason to choose either approach: at
571 claims/s, the engine clears any realistic claim volume in a fraction of a second, and the actual bottleneck in
the pipeline is the AI explanation step (1.8-90 s per call, four to five orders of magnitude slower than either rule
engine). Speed was never a legitimate reason to prefer YARA-X, and this benchmark settles that rather than leaving
it asserted.

**Fault isolation — tested three ways, including the fair one.** `src/yara_engine.py`'s per-rule loop wraps each
rule's evaluation in `try/except`; a crash is isolated to that one rule (`UNABLE_TO_ASSESS`, logged, the other 14
results unaffected) — already proven by the existing `tests/test_engine_robustness.py::IsolationTests`. Plain
`oracle.py` has no equivalent: its `evaluate()` is a one-line dict comprehension over `RULES`, so any single rule
function raising takes the whole claim's 15 results down with it. Injecting the identical fault used in that
existing engine test (`RULES['R007'] = lambda c, p: 1/0`) into all three variants gives:

| Variant | Result of the injected R007 crash |
|---|---|
| Plain `oracle.evaluate()` (the real code in the repo) | **Crashes.** All 15 results for the claim are lost, not just R007. |
| Hardened oracle (a five-line loop with `try/except` per rule, added for this test only) | Isolates it: R007 -> `UNABLE_TO_ASSESS`, the other 14 results match the no-fault baseline exactly. |
| `src/yara_engine.py` (unchanged) | Isolates it: R007 -> `UNABLE_TO_ASSESS`, same as the hardened oracle. |

The honest reading: isolation is not an inherent YARA-X property. A five-line wrapper gives plain Python the exact
same isolation semantics, and it is still ~11x faster with the wrapper in place. What does not transfer with that
wrapper is the deeper, structural guarantee: a YARA-X rule is a declarative pattern match that cannot make a network
call, write a file, mutate shared state, or loop forever, by construction of the rule language itself. A Python rule
function, hardened or not, is still an arbitrary function — nothing except the author remembering to keep it that
way stops a future rule from doing something it should not. That guarantee, not error-handling ceremony or raw
throughput, is the actual reason for choosing a declarative engine: it holds regardless of how carefully (or not)
the Python alternative is written.

Also considered and not decisive: lines of rule-definition code (`rules/core.yar` 577, `tests/oracle.py` 316 — the
oracle is shorter partly because it reuses Python's native comparisons where the engine matches pre-extracted,
percent-encoded fact strings) and per-rule versioning (`rule_version` on the compiled pack vs. a Python file's
change only visible as a whole-file diff) — both real but secondary to the two points above.

## 6. AI explanation step

**Role.** Explain each `FAIL` and `UNABLE_TO_ASSESS` finding in plain language for a human reviewer. Nothing else.

**Model in use (rounds two to four of the experiments):** `mistralai/Mistral-Nemo-Instruct-2407` on Featherless.ai, prompt v1.6.0 (`prompts/explain_findings.md`) with the closing gate on, `temperature=0`, `top_p=1`, `max_tokens=500`, 90 s timeout, one retry for transient failures only. The template (the engine's own sentence) is the floor.

**Prompt v1.6.0** asks for exactly three sentences, even when the finding is short: WHY ("Rule R0xx failed because ...", every engine reason in the model's own words), EVIDENCE ("The evidence shows ...", at least one value quoted exactly with its path), ACTION (a closing instruction). For the closing sentence the prompt is built per finding: the marker `[[CLOSING]]` is replaced by the verb and words of that rule's own `corrective_action` (trusted rulebook text), so the model is told exactly which action to state. It also tells the model to copy cited paths character by character and includes three worked examples, two with short findings. Prompt versions without the marker build exactly as before. It forbids approvals, clinical or fraud judgement, invented identifiers, relative-time claims, currency symbols and validity statements. Claim text and notes are labelled untrusted data. Older prompts are kept in `prompts/variants/` (`v1_3_0.md` is the round-one baseline, `v1_4_0.md` the round-two prompt, `v1_5_0.md` the round-three prompt).

**What reaches the model.** Only the validated finding, the rule excerpt and, if supplied, an untrusted note, with these limits: each string is cut at 1,000 characters, the note at 4,000, and a prompt over 60,000 characters fails closed to the template.

**Reply contract** (`ExplanationOutput`, pydantic, strict): `explanation` (1 to 1,500 characters), `cited_evidence_paths` (only paths present in the finding), `cited_rule_ids` (exactly the finding's rule), `needs_human_review` (a real boolean equal to the finding's flag). Unknown keys are forbidden.

**Validation, in the orchestrator for every provider** (`explain_with_fallback`):

| Check | Rejects |
|---|---|
| Schema | Wrong keys or types, unknown or missing citations, a changed review flag |
| Citation repair (before the schema check) | Not a rejection: a formatting slip in a cited path (a dropped letter, a stray space, a more specific path under an allowed one) is mapped to the one allowed path it means and recorded; the text, rule id and review flag are never touched; unmappable paths are still rejected |
| Grounding guard | Currency symbols, relative-time claims, unsupported validity assertions ("the values match correctly"), approval or payment language about the claim itself (enforced since the OWASP LLM01 battery in `tests/test_injection_owasp_llm01.py`; it rejects 0 of 3,926 real accepted answers), clinical/diagnostic/fraud judgement -- asserted in the prompt (docs/01's scope boundary) but not actively tested until this pattern; a retroactive scan of all 4,198 recorded live answers across every experiment round and every model found 0 real occurrences (one candidate phrase, "consistent with," was tried and dropped after the scan found a genuine false positive -- too common in ordinary English on its own) |
| Garbled-text guard | Text in another script, the Unicode replacement character, long repetitions (found by the experiments: garbled but schema-valid explanations were once accepted) |

**Closing gate** (`closing_retry`, on for the Featherless provider): when a valid answer does not state the rule's corrective action (at least half of the word stems of its first clause), the provider asks once more with a short correction; if the second call fails, is rejected or still lacks it, the first valid answer is kept. Roughly 6% to 9% of answers get one extra call. A provider receives private copies of the finding and rule, so it cannot edit the result it is explaining. Any failure produces the template answer and an explicit `used_fallback` with the error.

**Cascade** (`CascadeExplanationProvider`, optional). Tiers are tried in order; every tier gets a copy, is validated like a single provider, and the audit log names the tier that wrote the text and why earlier tiers were skipped. Enabled by `FEATHERLESS_FALLBACK_MODEL`; not the default.

**Audit of the AI.** `ai_request` (question, prompt hash, finding hash) is written before the call; `ai_recommendation` or `ai_failure` after it; every action type is `human_escalation`; `auto_correct_applied` is always false.

### 6a. Local models

`OllamaExplanationProvider` (`src/llm_adapter.py`) serves any model through a locally-running Ollama instance
(`http://localhost:11434`), fully offline: no API key leaves the machine, no per-call cost, and it goes through the
exact same schema, grounding and fallback path as every hosted provider — nothing about the safety net changes for a
local model. Motivation: the hosted Mistral-Nemo default costs money per call and needs internet access; a model that
runs entirely on the reviewer's own hardware removes both, which matters for an on-prem deployment. Three candidates
tested so far, on the RTX 4060 (8 GB) this was developed on, GPU placement confirmed live via `ollama ps` mid-call
(`100% GPU`, not a CPU fallback):

| Model | Works at default settings? | Live rate (84-case battery) | Notes |
|---|---|---|---|
| `gemma3:4b` (Ollama library) | **Yes** | 83/84 (98.8%) | 1.8-5 s per call warm; the one rejection is a genuine hallucinated rule citation (below), not a formatting slip or a garbled reply |
| `qwen3:4b` (Ollama library) | **No — disqualified, not just slow (below)** | Unusable: no `max_tokens` value tested worked across the 36-case set | Defaults to a hidden "thinking" mode that spends the token budget reasoning before writing an answer. Neither documented way to disable it (`/no_think` suffix, `chat_template_kwargs.enable_thinking=false`) works through Ollama's packaging of this model — verified, not assumed |
| `google/medgemma-4b-it` (official, gated, via `transformers` + 4-bit) | **Yes** | 30/36 (83.3%, 36-case tuning set) | The third-party GGUF (`unsloth/medgemma-1.5-4b-it-GGUF`) wrote a visible, unfenced `"thought\n..."` preamble and duplicated its own JSON answer with no separator -- a chat-template mismatch in that specific conversion, not a MedGemma problem: the *official* checkpoint (gated, `license: other`, Health AI Developer Foundations terms; `"gated": "auto"` so access is instant on acceptance) produces clean, correctly-fenced JSON with no preamble. Runs in 4-bit (`bitsandbytes`, NF4) at 3.2 GB VRAM. Needs `pip install -r experiments/requirements-local-models.txt`; loading an 8 GB checkpoint via `transformers`' memory-mapped load can hit a Windows "paging file is too small" error under memory pressure -- freeing RAM (not resizing the page file) was enough here |

**Stress test, `gemma3:4b`, the largest single-model battery run in this project** (`scripts/stress_test_local_model.py`):
all 84 exercise cases (25 supplied + 11 own injection variants + all four 12-case "fresh" confirmation rounds) run
once, plus 10 of them re-run 3x each at identical settings to check call-to-call consistency.

- **0 of 84 raw replies garbled** (checked every reply, live or rejected, against the same foreign-script/
  long-repetition patterns the grounding guard uses — not only the ones the guard happened to reject).
- **1 of 84 rejected**: `EX-25`/R013. The raw reply cited `"R999"` as the rule id and wrote "Rule R999 failed
  because..." — three of four characters different from the real `R013`, a genuine hallucination, not a near-miss
  typo `repair_citations` should or would fix. The schema's `Literal['R013']` constraint caught it and the reviewer
  was shown the template instead. Recorded as evidence the safety net works, not patched: a prompt tweak reacting to
  one model's one mistake on one case out of 84 is exactly the overfitting this project's grounding guards were
  built to avoid (they were only ever added from patterns that recurred across many observed failures).
- **9 of 10 determinism-check cases byte-identical across 3 repeats.** `EX-06` differed once; re-run 9 further times
  (5 with raw-reply capture) came back byte-identical every time, both the raw text and the parsed fields. Most
  likely explanation: the same `closing_retry` mechanism already documented above (a second call when the first
  valid answer omits the closing sentence) firing on one of the three original calls and not the other two — a
  designed, already-audited behavior, not model instability. Not conclusively provable after the fact, but it did
  not reproduce once in 9 further tries.

Reproduce: `python scripts/run_llm_explanations.py --provider ollama --model gemma3:4b --cases exercises/llm_explanation_cases.jsonl --output outputs/llm_explanations_gemma3.jsonl --no-scorecard` and
`python scripts/stress_test_local_model.py --model gemma3:4b`. Raw data: `outputs/stress_gemma3_4b.json`,
`outputs/llm_explanations_gemma3.jsonl`, `outputs/llm_injection_variants_gemma3.jsonl`.

**MedGemma, same 36-case tuning set** (`scripts/run_llm_explanations.py --provider medgemma`, `src/llm_adapter.py`'s
`MedGemmaExplanationProvider`, which reimplements the same closing-gate retry the HTTP-based providers get built in,
for a fair comparison since this one is not an HTTP call):

- **30/36 live (83.3%)**: 22/25 supplied, 8/11 injection variants.
- **Two rejections were a real reliability finding, not a quality one**: `RuntimeError: p.attn_bias_ptr is not
  correctly aligned` -- a known `bitsandbytes` 4-bit attention-kernel alignment issue, non-deterministic by input
  shape. This is specific to the 4-bit quantized deployment path, not evidence about MedGemma's answers themselves.
- **It also gets fooled by the same injection case that fools `Mistral-Nemo`**: `VAR-02` flipped the review flag on
  MedGemma too (`ValueError: Review boundary changed`), exactly the injection variant the README already documents
  as fooling the hosted model -- caught by the schema guard both times, never reaching a reviewer.
- **One rejection was the same case that also fools `gemma3:4b`**: `EX-25`/R013 (`Unknown rule citation`) -- worth
  noting since it suggests this specific case is a harder case in general, not only a `gemma3:4b` weakness.
- **Median latency ~12.9-13.0 s, worst case 26.4 s** -- roughly 4x `gemma3:4b`'s median, on the same GPU, 4-bit vs.
  4-bit, so the gap is architectural (a larger effective compute path per token), not a quantization artifact either
  side is missing.

Reproduce: `python scripts/run_llm_explanations.py --provider medgemma --cases exercises/llm_explanation_cases.jsonl --output outputs/llm_explanations_medgemma.jsonl --no-scorecard` (needs
`pip install -r experiments/requirements-local-models.txt` and the gated license accepted). Raw data:
`outputs/llm_explanations_medgemma.jsonl`, `outputs/llm_injection_variants_medgemma.jsonl`.

**Qwen3, disqualified -- not on quality, on unprovisionable resource requirements.** Requested by name (the mentor
asked for it explicitly). Three separate runs against the same 36-case set, escalating the one setting that should
fix a "thinking" model running out of budget:

| Attempt | `max_tokens` | Result |
|---|---|---|
| 1 | 500 (system default) | 100% failure: every case hits `finish_reason: length` with empty `content` -- confirmed reproducible, not a fluke, on a clean-memory retry |
| 2 | 3,000 | Failed differently on the very first cases tried (`JSONDecodeError`, empty content) even with ~5.7 GB of free RAM confirmed at the time -- not a memory-pressure artifact. Direct diagnosis on one specific failing case (`CG-A5FE8740EAE1`/R002) found it needed **4,613 completion tokens** to finish -- past the 3,000 budget that worked on a different, simpler case earlier |
| 3 | 8,000 | Different cases failed differently again: `ValueError: Invalid explanation keys` (a real schema-shape rejection) and `APITimeoutError: Request timed out` (generation exceeded the 90 s provider timeout even at this budget) |

**No single `max_tokens` value tested works across the case set.** Some cases finish under 3,000 tokens; at least one
needs 4,600+; others exceed 8,000 tokens and a 90-second timeout without finishing. This is a materially worse
problem than being merely slow (`medgemma-4b-it`'s latency is high but *bounded and predictable*): Qwen3's resource
requirements vary unpredictably per case, so there is no configuration that can be provisioned for in advance.
Neither documented way to disable its thinking mode (`/no_think` suffix in the prompt, `chat_template_kwargs.
enable_thinking=false`) works through Ollama's packaging of this model. **Verdict: not usable in this pipeline as
currently packaged**, independent of answer quality -- an unmeasurable variable is not the same finding as a bad
score.

**Automated-metric comparison, all four candidates**, same 36 tuning cases, same scorer
(`scripts/evaluate_ai_explanations.py`), same prompt version (v1.6.0 + closing gate):

![Model comparison: gemma3:4b, medgemma-4b-it, Mistral-Nemo, qwen3:4b — live rate and latency](figures/model_comparison.png)

| | `gemma3:4b` (local, free) | `medgemma-4b-it` (local, free) | `Mistral-Nemo-Instruct-2407` (hosted, paid) | `qwen3:4b` (local, free) |
|---|---|---|---|---|
| Live answer rate | **97.2%** (35/36) | 83.3% (30/36) | 94.4% (34/36) | Disqualified -- no stable config |
| Unsupported-token candidates | 0 | 0 | 0 | -- |
| Injection: approval language leaked | 0 | 0 | 0 | -- |
| Median latency | **~3.0 s** | ~12.9-13.0 s | ~4.1 s | Unbounded (3 s-90 s+) |
| Worst-case latency | **10.8 s** | 26.4 s | 26.2 s | Exceeds the 90 s timeout |
| Also has a stress-test track record | **Yes, 84 cases, 0 garbled** | No, 36 cases only | N/A (hosted, chosen by 4 rounds) | N/A (disqualified before one was run) |

**`gemma3:4b` is the winner**, ahead of every other candidate on every automated metric measured, including the
currently-chosen paid hosted model, and it is the only local candidate with a large-scale stress-test track record
behind it. `medgemma-4b-it` ties on safety (grounding, injection resistance) but is clearly behind on reliability
and speed; not disqualified, just not the stronger candidate. `qwen3:4b` is disqualified on operational grounds
before quality was ever assessed. **This table is still not a verdict on substance.**
`evaluate_ai_explanations.py`'s own docstring is explicit that automatic checks do not replace the manual 0/1
scorecard (correct finding, correct evidence, correct rule, appropriate action, honest uncertainty) -- that still
needs a human, for every model in this table, hosted or local (the README already notes nobody has done that pass
yet). What can honestly be said today: `gemma3:4b` is the strongest candidate on every automated axis measured, free
and local besides.

**The manual scoring pass itself cannot be done by an AI**, including this one: an LLM scoring another LLM's
answers for correctness is exactly the circularity independent verification exists to avoid. What this project can
honestly do is prepare the sheet: `experiments/manual_scoring_sheet_gemma3_rescore.csv`
(`scripts/export_scoring_sheet_gemma3.py`) holds `gemma3:4b`'s real answers on the same 36 cases originally prepared
for the Mistral-Nemo backlog (`_e8`, `_e9b`, `_e10b.csv`, section 11 -- prepared, never actually scored). Since
there is only one model in this sheet, there is no "arm" to hide, so it skips the blind key those sheets used and
shows the case id directly; case order is still shuffled to avoid a round-order effect. This is the one piece of
evidence in the whole evaluation that a human, not a script or a model, has to actually produce.

## 7. Audit log

| Element | Specification |
|---|---|
| Chain | Each row: `sequence`, `recorded_at`, `previous_hash`, `event`, `hash` (SHA-256 of the canonical row); the first `previous_hash` is 64 zeros |
| Anchor | `<log>.head.json` holds the latest hash and count; detects truncation and replacement |
| Keyed anchor | With `AUDIT_ANCHOR_KEY` set the anchor carries an HMAC-SHA256; without a key, whoever can write the files can rewrite chain and anchor together (tested and documented) |
| Concurrency | Thread lock and OS file lock; the tail is re-read on every append; the anchor is replaced atomically |
| Durability | `ai_request` events are fsync'd |
| Encoding | Rows are ASCII-escaped JSON so no character (U+2028, U+0085) can split a record |
| Verification | `scripts/verify_audit.py` checks chain, anchor (strict by default: rows after the anchored position are rejected; `--allow-unanchored` for a log still being written) and AI ordering; with `--results` it re-checks every result hash |

**Event types:** `ingestion`, `run_started`, `rule_check` (status, severity, `result_hash`, confidence fields), `ai_request`, `ai_recommendation` (`model` is the tier that answered; `citation_repairs` lists any repaired paths), `ai_failure`, `system_decision` (`route_to_human_review`, `no_findings_for_review`, `quarantine_claim`; never an approval), `run_finished` (rule pack hash and engine code hash), `recheck_run`, `duplicate_submission`, `advisory_check`, and the reviewer decisions. A claim id submitted again outside the recheck flow is recorded as a duplicate and routed to a human.

The log is tamper-**evident**, not immutable; `docs/16_Audit_Log_Design.md` lists what production immutability would add.

## 8. Human review workflow

`src/make_review.py` builds an offline page (status and text filters, evidence as submitted, decisions downloaded as JSONL). `src/review_workflow.py` validates decisions (real status, reviewable findings only, reason and actor required, one bad decision rejects the batch), counts unresolved findings, and `recheck` runs a corrected claim as a new run with its own audit trail while leaving the original untouched. Reviewer identity is self-declared; there is no authentication yet.

## 9. Security

Audited against the OWASP Top 10 for LLM Applications (2025) and the OWASP Top 10 (2021); details, tool results and residual risks in `docs/20_Security_Audit.md`.

| Control | Where |
|---|---|
| Model output is never trusted | Section 6 validation; provider copies; cascade validates every tier |
| Injection through claim data | Percent-encoded facts; untrusted labelling and fencing in the prompt; review page writes text only |
| Unbounded consumption | Prompt limits, `max_tokens`, timeout, one retry, bounded concurrency |
| Secrets | `.env` git-ignored and scanned; the key never enters a prompt, log or experiment record |
| Supply chain | Exact pins; `pip-audit` clean; no `eval`, `exec`, `subprocess` or `pickle` in our code (a test scans) |
| Log integrity | Hash chain, anchor, optional HMAC; log forging prevented with `%r` logging; `verify_audit.py` reports whether the anchor is signed and `--require-key` enforces it |
| Hostile input | Property-based fuzzing of ingestion, the model reply gate, the audit log, the engine, injection, the review page and the reviewer API (sections 10a, 10d) |
| Identity and access | Badge, password and authenticator login; four clearance levels; hide-not-disable responses with masked identifiers; decisions bound to the session; signed security audit log; 21 attacks and 6 races on two stores (section 10d) |

Residual: the offline file-based review flow has no authentication (the reviewer API does, section 10d), protected health information would go to a third-party model with real data, the audit log is not immutable storage, and there is no per-run spending cap.

## 10. Verification

| Layer | Result |
|---|---|
| Organizers' answer key | 9,000 of 9,000 results; precision, recall and status accuracy 1.0 on all three splits |
| Handbook worked cases | 10 of 10 exact |
| Independent oracle (`tests/oracle.py`) | **0 disagreements** over 107,635 generated claims (`scripts/status_coverage.py`), plus 37,000 boundary-aware mutants and 123 hand-derived boundary cases |
| Hostile inputs | Runner, ingestion, review page, audit log, AI providers |
| Phase 2 evaluation | Nine cited example sets in three evidence tiers (organizer key, independent oracle, hand-derived); F1 1.0 on the three public splits, no valid claim flagged, FHIR path 0.9745 (R009 abstains); `docs/29_Test_Evaluation_Report.md`, evidence in `outputs/evaluation/` |
| Fuzzing | Six trust boundaries, 25 property tests at 3,000 generated examples each plus one regression test, all passing; two defects found and fixed (section 10a) |
| Security and red team | `docs/20` |
| Suite | 1019 tests, offline (420 of them verified in CI on Python 3.10, 3.12 and 3.14; all 1019 passed locally on all three in fresh environments built from `requirements-dev.txt`, 2026-10-04); the committed audit sample's 6,000 result hashes are re-checked |

**Independent oracle at scale.** `tests/oracle.py` reimplements the 15 rules from `rules/rules.json` and `docs/04` alone; it imports nothing from `src/`, so the engine and the oracle cannot share a bug by construction — a mistake would have to be made independently, the same way, in both. `scripts/status_coverage.py` generates 107,635 claims with `tests/claim_gen.py` (half seeded from a fully valid claim then randomly damaged, half fully independent-random fields), scores each with both the engine and the oracle, and hard-fails on the first disagreement rather than only counting them, so the artifact below is either "0 disagreements" or the run did not complete:

![Every rule vs. the independent oracle: 0 disagreements over 107,635 generated claims](docs/figures/oracle_agreement.png)

The same run's per-rule status coverage: every rule reaches every status it can by design (PASS, FAIL, UNABLE_TO_ASSESS, and NOT_APPLICABLE only for R008/R009/R010/R014), including R009 PASS at 23,627 of 107,635 — a status a naive fuzzer that randomizes every field independently can miss even at this scale, because R009's PASS needs five fields (patient, service code, status, date range, quantity) to agree on one claim at once. Seeding half the batch from a known-valid claim first is what makes it reachable at a usable rate.

![Status coverage per rule across 107,635 generated claims, with what each status means](docs/figures/status_coverage.png)

Raw counts: `outputs/status_coverage.json`. Full writeup, including the fuzzer gap that motivated this: `docs/19_Stress_Testing_and_Judging_Coverage.md` section 2b.

### 10a. Fuzz testing

Property-based tests generate hostile input for the six places where untrusted data crosses a boundary and check an invariant that must always hold. They live in `tests/test_fuzz_*.py`, share profiles and strategies in `tests/fuzz_strategies.py`, use Hypothesis (dev-only, `requirements-dev.txt`), mock the model, and need no network. `FUZZ_PROFILE=ci` (the default) is a fixed seed with 60 examples per test; `deep` is selected by `scripts/fuzz_campaign.py`.

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

How to run: `python -m unittest discover -s tests -p "test_fuzz_*.py"` for the fast profile, `python scripts/fuzz_campaign.py --examples 3000` for the deep run. Design: `docs/superpowers/specs/2026-10-04-fuzz-testing-design.md`.

### 10b. Phase 2 detection evaluation

**Why.** The Phase 2 rubric asks for detection quality on a 50-claim validation set, with a high macro F1 across rule categories and valid claims preserved. The public splits already score 1.0, so a single number on them proves little. The evaluation therefore scores every labelled example set the repository can supply, says where each label came from, and reports the evidence strength of each result separately. The full report is `docs/29_Test_Evaluation_Report.md`; this section is the specification of how it is produced.

**Example sets and evidence tiers.** Labels differ in trustworthiness, so sets are grouped in three tiers and never pooled into one headline number.

| Tier | Set | Claims | Where the label comes from |
|---|---|---|---|
| A: organizer answer key | S1 `data/development` | 400 | `expected_results.jsonl`, organizers' dataset v1.0.0 |
| A | S2 `data/validation` | 150 | same |
| A | S3 `data/stress` (the 50-claim split) | 50 | same |
| A | S4 handbook worked cases | 0 new | the ten cases are all already in S1 to S3, so they add no claims |
| A | S5 the 600 public claims as FHIR bundles | 600 | organizer key for the same claim id; tests the FHIR importer |
| A | S6 the 600 public claims as CSV folders | 600 | organizer key; tests the CSV importer |
| B: independent oracle | S7 generated claims (`tests/claim_gen.py`, seed 20260927) | 107,635 | `tests/oracle.py`, a second implementation written from the rulebook |
| B | S8 boundary-aware mutants of the public claims (seed 20261004) | 33,945 of 37,000 attempts | oracle |
| C: hand-derived | S9 boundary table `tests/test_stress_boundaries.py` | 123 cases, 140 labelled results | derived by hand from the rulebook wording |

Tier A is the only external truth. Tier B and C measure agreement between implementations and expectations written by the same team from the same text; a shared misreading would not show, so they are reported as agreement, never as accuracy. Every set is recorded in `outputs/evaluation/provenance.json` with its files and their SHA-256, generator and seed, label source, counts, known limitation and the commit that produced the evidence. A claim in more than one tier-A set is counted once (dedupe compares content, not the claim id). Claims the importer would quarantine are dropped from S7 and S8 and counted (108,726 generated, 107,635 scored).

**Metrics.** The positive class is `FAIL`, as in the organizers' scorer. `UNABLE_TO_ASSESS` is scored separately because an unnecessary abstention costs reviewer time without being an error.

- *Rule categories.* The rulebook defines none, so we group the 15 rules by what they check: completeness and arithmetic (R001, R007, R012); eligibility and coverage (R003, R004, R005, R015); timing (R002, R014); authorization and documentation (R008, R009, R010); catalogue, pricing and duplicates (R006, R011, R013). Counts are pooled within a category and F1 is averaged across categories where it is defined. Macro F1 is also given by severity (11 high, 4 medium) and over all 15 rules so the grouping cannot flatter the result. A category or rule with no failing example has an undefined F1; it is excluded and listed, never scored as 0.
- *Valid claims.* The claim-level false-positive rate is the share of claims with no `FAIL` in the key where the engine raised any `FAIL`. For fully clean claims (every result PASS or NOT_APPLICABLE) the report also gives the share where the engine abstained and the result-level false-alarm rate.
- *Uncertainty.* With zero errors a point estimate misleads, so each rate carries an exact one-sided 95% upper bound (Clopper-Pearson; Wilson above 2,000 errors). By the rule of three, claiming an error rate below 1% needs about 300 error-free examples.

**Results.**

| Set | Claims | Results | Precision | Recall | F1 | Status accuracy | Disagreements |
|---|---|---|---|---|---|---|---|
| S3 `data/stress` (50 claims) | 50 | 750 | 1.0 | 1.0 | **1.0** | 1.0 | 0 |
| S2 `data/validation` | 150 | 2,250 | 1.0 | 1.0 | **1.0** | 1.0 | 0 |
| S1 `data/development` | 400 | 6,000 | 1.0 | 1.0 | **1.0** | 1.0 | 0 |
| S6 CSV folders | 600 | 9,000 | 1.0 | 1.0 | **1.0** | 1.0 | 0 |
| S5 FHIR bundles | 600 | 9,000 | 1.0 | 0.9502 | **0.9745** | 0.9656 | 310 (see 10c) |

Macro F1 is 1.0 by category, severity and rule on S1 to S3 and S6. On S5 it is 0.9535 by category, 0.9820 by severity and 0.9333 over rules. In the 50-claim split 9 of the 15 rules have no failing example, so their F1 is undefined there and the timing category is excluded from its macro; the smallest non-zero count is 3. That split is thin evidence by itself, so per-rule conclusions rest on S1 (at least 10 failures per rule) and S2 (at least 4).

*Preserving valid claims.* The engine raised a `FAIL` on none of the claims whose key has no failure: 0 of 160 (95% upper bound 1.85%) in development, 0 of 62 (4.72%) in validation and 0 of 24 (11.73%) in the 50-claim split. Among fully clean claims it raised none and abstained on none (0 of 136, 53 and 12), except on the FHIR path (10c). Result-level false alarms on clean claims: 0 of 2,040, 795 and 180.

*Baselines, to show the metric discriminates.* Predicting PASS everywhere scores F1 0 (status accuracy 0.836, 0.835, 0.799). The organizers' starter baseline, which implements 3 of 15 rules, scores F1 0.432, 0.450 and 0.595 with recall 0.276, 0.291 and 0.423 on development, validation and stress.

*Larger and harder sets.* 0 disagreements with the oracle on 107,635 generated claims (1,614,525 results) and 33,945 mutants (509,175 results), and with the hand-derived expectation on all 140 boundary results. No isolated engine crash occurred in any set. Also none of the 600 public claims violates any of the extra rules we considered (a diagnosis outside the catalogue, a payer that differs from the policy's, a reused invoice number), which is why those cannot be demonstrated on public data.

*Latency* (Windows 10, Python 3.10, 16 CPUs, one process; machine-dependent). Rule engine per claim: median 0.66 ms, p95 1.11 ms, p99 1.31 ms. With FHIR import 0.78 ms median, with CSV import 0.69 ms. The audited path with the deterministic template, which includes the hash-chained log write with its anchor update, has a median of 28.0 ms and p95 of 71.2 ms. The hosted AI explanation step is quoted from earlier live runs (median 2.86 s, p95 28.89 s, 117 calls), not re-run, and is not part of the detection metrics.

**Reproduce.** `python scripts/evaluate_phase2.py` (about six minutes) writes `outputs/evaluation/metrics.json` and `provenance.json`; accuracy sections are deterministic (fixed seeds), latency is not. `python scripts/render_eval_report.py` fills the tables in `docs/29`, and `--check` fails if the report and the evidence disagree. Tests: `tests/test_eval_metrics.py`, `test_eval_sets.py`, `test_eval_sets_generated.py`, `test_evaluate_phase2.py`, `test_eval_report.py`.

**Limits.** A perfect public score cannot rank this system against a better one: the answer key is deterministic and the claims were available while the rules were built. The oracle shares our reading of the rulebook. Tier C is small and written by us. The rule categories are our grouping. The claims are synthetic and say nothing about how often real claims fail. Latency is one laptop, not a load test. Nothing here evaluates review decisions or explanation quality (`docs/17`, `docs/21`).

### 10c. Why FHIR claims do not reach F1 1.0

**Observation.** The same 600 claims score F1 1.0 when read directly or from CSV folders and 0.9745 when read from FHIR bundles. Precision stays 1.0 and recall falls to 0.9502 (439 of the 462 failures in the three splits), status accuracy is 0.9656, macro F1 by category is 0.9535. Of the 9,000 results, 310 differ from the answer key. The disagreement table in the evidence lists every difference in every set, and this is the only place any appear:

| Rule | Key says | Engine says | Count |
|---|---|---|---|
| R009 | PASS | UNABLE_TO_ASSESS | 287 |
| R009 | FAIL | UNABLE_TO_ASSESS | 23 |

**Cause, step by step.**
1. R009 (`docs/04`) resolves each line's authorization id in the supplied authorizations and checks patient, service code, status `approved`, valid-from and valid-to dates, and the total quantity against the maximum. It needs the whole record.
2. The FHIR bundles in this dataset contain 600 `Patient`, 1,200 `Organization`, 600 `Coverage`, 600 `Claim` and 372 `DocumentReference` resources. There is no `ClaimResponse` or other resource holding an authorization record. The only trace is the reference number in `Claim.insurance.preAuthRef`, present in 327 bundles.
3. `src/fhir_adapter.py` maps what exists. It keeps the reference and builds a stub (`authorization_id` set, patient, service, status, dates and quantity empty). On a real R009 failure, the normalized claim has one full authorization and the FHIR version has one stub with the same id and nothing else.
4. The rulebook says that a missing comparison input leaves the rule `UNABLE_TO_ASSESS`. With the stub, R009 cannot compare anything, so it abstains. The answer key was computed from the full claim, so it says PASS or FAIL. The two can never agree on these 310 results from the bundle alone.

**What the number does and does not mean.** The abstention is the correct, fail-safe answer: the 23 real failures go to a reviewer and **none became a PASS** (no result in any set moved from FAIL to PASS). The cost is workload, not safety: 134 of the 201 fully clean claims get an unnecessary R009 abstention on this path. The engine, the rules and the CSV path are unaffected. A false-positive rate is not involved: 0 of 246 claims without a failure were flagged on the FHIR path as well.

**Why we do not make it 1.0.** Reaching 1.0 from the bundle alone would require inventing the missing record. A guess that is wrong turns some failures into silent passes, which the non-negotiable checks forbid (an unknown check is never shown as PASS). Rejecting every such claim would flood reviewers. Abstaining is what a careful human would do.

**What would reach 1.0 honestly.** Resolve the reference against a payer or authorization registry at ingestion, which is how a real deployment would work and is where an integration with the payer's system belongs. In FHIR R4 a prior authorization is normally carried as a `Claim` with `use` set to `preauthorization` and its `ClaimResponse`; this is stated from general FHIR knowledge and was not checked against the specification text in this project. The organizers' bundles contain neither. Either route is future work, not something the dataset lets us measure.

**Reproduce.** `python scripts/evaluate_phase2.py` and read set S5 in `outputs/evaluation/metrics.json` (`summary.disagreements.breakdown`); `docs/29` section 4 renders the same table. The earlier experiment `defense_experiments.py ingestformats` (`docs/27`) reported the same effect before this evaluation existed (FHIR agreement 96.5% to 96.9%, every difference R009, no silent pass).

### 10d. Identity and access: the reviewer API

**Why.** Until now the identity of a reviewer was text in a decision file: anyone who could edit the file could claim to be anyone
(`docs/20`, A07; the "no identity" finding in the security review). The reviewer API closes that for everything that goes through
it. The design (`docs/superpowers/specs/2026-10-04-identity-access-design.md`) follows the team's Attijari login: a badge number,
a password and an authenticator-app code, then clearance levels with per-user permission flags, and responses that omit what the
caller may not see. It differs from Attijari where that was weaker: it fails closed when its store is down, has no blanket
administrator bypass, binds a CSRF token to the session, and answers every failed login identically.

**Login.** `POST /api/v1/auth/login` takes `badge_id` (`CG-` and 4 to 8 digits), `password` and the 6-digit `totp`. The password
is checked with bcrypt (cost 12 in production); an unknown, locked or inactive account spends the same time on a dummy check, so
neither the response nor the clock tells an attacker which badges exist. The code is for exactly the current 30-second step and
is consumed through a unique database index, so it works once, even when 50 requests race with it. Five wrong attempts lock the
account for 15 minutes (a lock is never extended by further attempts). A session is a signed token in an `HttpOnly`,
`SameSite=Strict` cookie plus a CSRF token that every state-changing request must echo. The token carries only the badge and a
session id: **level and permissions are read from the database on every request**, so a demotion or deactivation applies to the
next request and a forged "role" inside a valid token changes nothing. If the database is unreachable, login fails; nothing is
accepted on a guess.

**Clearance levels.** Permissions are flags; a level is a default set. Separation of duties is deliberate: level 4 manages users
and reads the audit log but cannot read or decide claims, so the person who administers the system is not the person who approves
its findings.

| Permission | L1 viewer | L2 reviewer | L3 senior | L4 admin |
|---|---|---|---|---|
| `claims.view` (findings, evidence, explanations, masked) | yes | yes | yes | no |
| `claims.view_notes` (claim notes and attachment text) | no | yes | yes | no |
| `claims.decide` (confirm, dismiss, request information) on medium-severity findings | no | yes | yes | no |
| `claims.decide_high` (the same on high-severity findings) | no | no | yes | no |
| `claims.recheck` | no | yes | yes | no |
| `pii.unmask` (real identifiers for one claim, with a reason; logged) | no | yes | yes | no |
| `audit.view`, `audit.verify`, `users.manage` | no | no | no | yes |

An administrator can set another user's level up to their own, grant or revoke single flags, and unlock accounts, but cannot change
their own level, deactivate themself, demote the last active administrator, or give user-management and audit flags to anyone who
is not level 4.

**What the API does with a request.** Every endpoint checks the session, then the CSRF token on unsafe methods, then the
permission. A caller who lacks a permission gets 403 and an audit event; no session gets 401. Responses are shaped for the caller:
patient and member identifiers are replaced by stable keyed pseudonyms (also inside evidence values and explanation text), notes
and attachment text are removed below L2, and the per-finding `allowed_actions` list is present only when the caller could
actually perform one. Before a masked response is sent it is searched for any surviving raw identifier; if one is found the request
fails instead. A decision is built by the server: the reviewer is the session's badge, the finding's current status is read from
the results, the time is the server's, and a request body carrying any of those fields is rejected. A reviewer (L2) is refused on a
high-severity finding; only L3 can decide it, which is the explicit senior approval the human-in-the-loop line asks for. Bodies are
limited to 64 KB, must be JSON, use strict types and refuse unknown fields; validation errors never echo the input; unexpected
errors become a plain 500; every response carries `Cache-Control: no-store`, `nosniff`, `X-Frame-Options: DENY`, `no-referrer` and
a `default-src 'none'` policy. **Not yet built:** `POST /claims/{id}/recheck` checks its permission and then answers 501, because a
real recheck needs stored run traces and result versions, which arrive when claims move into the database with the task queue.

**Audit.** A second hash-chained log records logins and failures (with the true reason, which the caller never sees), lockouts,
logouts, rejected tokens, refusals, unmasking, decisions and every user change. Its anchor must be HMAC-signed and the server
refuses to start without the key. Events cannot carry anything whose name suggests a secret, values are bounded and JSON-safe, and
floods of failures, refusals or bad tokens are capped per minute and summarised, so the log cannot be filled by an attacker.

**How it was tested.** 289 tests, run on Python 3.10, 3.12 and 3.14.
- *One contract suite, two stores.* The in-memory store and a real MongoDB 7 pass the same 20 tests, including 50 parallel
  submissions of one code (exactly one succeeds) and 500 parallel failed-login updates (none lost).
- *A permission matrix* of every endpoint against no session and each level (85 cells), written out as data.
- *An attack suite, 21 attacks, on both stores:* brute force and credential stuffing, a spoofed forwarded-for header, replay,
  forged, tampered, expired, downgraded and `alg: none` tokens, privilege claims inside a valid signature, session reuse and
  fixation, operator injection (`{"$ne": null}` and others) in every field and path, mass assignment and escalation, malformed
  and raw hostile requests sent as exact bytes, enumeration by response and by time, and data exposure. All hold.
- *A race suite, 6 races on both stores,* including a demotion that must be visible to every request that starts after it.
- *Fuzzing:* Hypothesis on the API (6 tests); a deep run of 1,500 examples per test passed in 104 seconds.
- *Mutation checks:* 25 deliberate breakages (a missing dummy password check, an accepted replay, ignored revocation, a missing
  CSRF check, a missing severity gate, unmasked responses, a permission cache, a non-atomic code check and others) are each caught
  by a test. Running them found three weak tests of mine (a timing test too cheap to see a missing dummy check, a self-demotion
  guard hidden behind the last-admin guard, a masking test that used the code under test to list identifiers); all three were
  strengthened. The contract suite found one design flaw before any code depended on it: a user field literally named `badge_id`
  collided with the identifier argument of the update call, which is now positional-only.
- *A real run:* `python scripts/access_experiments.py all` starts the actual server on a real port with MongoDB and drives it over
  HTTP: **25 of 25** checks passed (`outputs/defense/access.json`).

**Load** (one laptop, Windows 10, 16 CPUs, server and clients in one process, MongoDB 7.0.43 in Docker, production password cost):

| Clients | Login p50 / p95 | Logins per second | Read p50 / p95 | Reads per second | Errors |
|---|---|---|---|---|---|
| 1 | 336 / 369 ms | 3.0 | 8.6 / 17.3 ms | 97 | 0 |
| 8 | 334 / 367 ms | 23.2 | 53.4 / 86.9 ms | 138 | 0 |
| 32 | 662 / 1,035 ms | 42.5 | 192.6 / 253.7 ms | 159 | 0 |

A login costs about a third of a second because bcrypt is meant to; it scales to about 40 per second here, near what 16 cores of
hashing allow. Reads saturate near 160 per second because client and server share one Python process; a separate load generator
would show more. These numbers describe this machine, not a deployment.

**Limits and residual risks.**
- The login throttle (20 failures per address per 15 minutes) lives in one process. Behind a reverse proxy every client shares the
  proxy's address unless the proxy is trusted explicitly, which is not built; with several server processes each has its own count.
- Changing a password ends the session that changed it, not the user's other sessions, which live until they expire (60 minutes).
- The check that stops the last administrator being removed reads a count and then writes, so two administrators removing each
  other at the same instant could leave none. A stored update cannot repair that; recovery is the command-line tool.
- There is no password-reset flow: an administrator can unlock an account and reset its authenticator, but a forgotten password
  needs an account to be recreated. There is no self-service sign-up, no single sign-on, and no per-claim assignment yet (every
  reviewer with the permission can open every claim; assignment arrives with the task-queue work).
- The server does not terminate TLS itself beyond uvicorn's options; the cookie is marked Secure, so a real deployment sits behind
  TLS. The compose file is for local use: the database is bound to localhost, not encrypted in transit.
- Secrets come from the environment; dev mode keeps generated ones in a git-ignored file. The authenticator seed is encrypted with
  one key and there is no rotation procedure. The audit anchor key lives in the server's environment, so someone who controls that
  environment could forge the log (`docs/20`, F4).
- The "ID badge" is a number typed at login, not a hardware badge; the second factor is the authenticator code.
- The file-based review flow of Phase 1 (`scripts/run_audited_review.py`, the offline review page) is unauthenticated by design and
  remains for offline demonstration; only the API binds decisions to a verified identity.
- Claims and results are still read from files, and recheck is not implemented in the API (501).
- *Found by the final independent review and fixed before merge.* An administrator could grant themself `claims.decide_high` through
  the user-update call, defeating the rule that whoever administers the system cannot approve its findings: level 4 can now hold only
  the three administration flags, whoever asks, and a stored claim grant on a level 4 record is not honoured. Masking compared
  identifiers case-sensitively, and so did its own leak check, so a note saying `ABC123` for patient `abc123` passed both: both are now
  case-insensitive. The review state re-read the whole decision log on every request; it is now parsed once per change of the file.
- The security log's event listing (administrators only) still re-reads its file per request, which is acceptable at audit volumes.
- A code is accepted only in the exact 30-second step in which it is shown, so a code typed in the last second of a step is refused
  and the user simply tries the next one; widening the window would weaken the replay guarantee.

### 10e. Extension rules (advisory)

**What and why.** The mentor said there are more than fifteen rules and gave fifteen. To show the team did not take them as given, eight
further deterministic checks were built from real claim-validation practice (CMS NCCI and MUE, WEDI SNIP, ICD-10-CM sequencing, CARC 18,
cumulative authorization tracking). They are advisory: their own pack (`rules/extensions.yar`, generated from the catalogue and kept
identical by a test), result schema (`schemas/extension_result.schema.json`), output file and ids (`E001` to `E005` on one claim,
`E101` to `E103` using earlier claims; the `E` prefix avoids the mentor's probable `R016` and up). They never change the official
verdict. Full rule cards, sources and limits: `docs/30_Extension_Rules.md`.

**Separation, proved by tests.** The ten official engine, rule and schema files are byte-for-byte what they were before the work
(SHA-256, insensitive to line endings), and the official results on all three public splits hash to the values recorded before it.

**Honesty about the data.** The teaching catalogue has six services and six diagnoses, with no pair tables, drug flags or accident
flags. The rule logic is real; the attributes are invented, live in a catalogue labelled fictional, and use codes outside the teaching
catalogue. This was forced by measurement: every pair of real services occurs together on one date in 36 to 54 public claims, so a
pair rule on real codes would flag dozens of valid claims. Consequently E001 to E005 cannot fire on public data, and the three
cross-claim rules cannot either (every one of the 600 patients has one claim); all are verified on constructed scenarios.

**"Earlier claim"** is defined once: same non-empty patient, a different claim id, ordered strictly before by (submission date, claim
id). A claim's result therefore does not depend on later claims. No history, or a history store that returns anything but a list of
claims, gives `UNABLE_TO_ASSESS`, never a pass.

**Findings during the work.** (1) The first public-data run showed E102 flagging 8 claims that the official R009 already fails (a single
claim's own excess); it now fires only when earlier claims contributed units, as E103 does. (2) The independent oracle first accepted
only whole-number quantities while the public data has 1.5; 18 of 4,800 comparisons disagreed until the oracle was fixed. (3) Fuzzing
found a history store returning an empty dict was read as "no earlier claims"; it now fails closed. (4) The coverage guard on the
generated oracle test caught that the first generator never produced an E002 failure.

**Final independent review** (a fresh reviewer on the whole branch) found two real defects, fixed test-first: a negative quantity could
cancel a genuine cross-claim excess (E102, E103 turned FAIL into PASS), and an earlier claim with no submission date sorted as the earliest
of all. It also found the spec still naming `SVC-PHARM` for E004, a missing deterministic test for FAIL outranking UNABLE, and
undocumented exact matching of authorization ids; all are corrected. Re-running the oracle test repeatedly then exposed a genuine
disagreement (an unreadable earlier quantity: E103 reported FAIL on the readable part while the oracle said unable); E103 now skips such a
group, as E102 does.

**Tests.** An independent oracle compared on 500 generated claim-and-history worlds; scenario tests with the status fixed by
construction, boundary cases included; Hypothesis fuzzing (arbitrary JSON claims and histories, hostile text, a misbehaving history,
large inputs), also in `scripts/fuzz_campaign.py`; 21 mutation checks of the tests, 20 caught and one equivalent mutant; a forgery test
(an authorization id imitating fact tags creates no foreign finding).

**Evidence** (`outputs/evaluation/extensions.json`): 0 findings on the 600 public claims; engine and oracle agree on 4,800 of 4,800
results; 29 of 29 constructed scenarios (95% upper bound on the disagreement rate 9.8%); about 5 ms per claim with 50 earlier claims,
about 9 ms with 10,000 other claims in the history.

**Limits.** Sources for E004 (route of administration) and E102 (NPHIES cumulative tracking) could not be confirmed independently; the
cards say so. The pair table, modifiers and event-date wording are our choices. Advisory findings are not yet shown in the reviewer
API or used by routing; that belongs to the queue work, which will also replace the in-memory history with a database-backed one.

### 10f. Task queue and work dispatcher

**What and why.** The mentor asked for a task queue (Celery or ARQ) and a way to give work to people. The idea behind it, ours: with 500
claims and 20 agents, each agent should see only their own slice (25), dealt fairly and reproducibly, with a person on every decision.
`src/workqueue/` holds a pure Python core (triage, states, dispatcher, explanation step, circuit breaker, replay, reconcile, shadow mode)
behind a store interface with an in-memory twin and a MongoDB implementation, both proved by one contract suite. Celery is a thin
adapter, Redis is only its broker, MongoDB is the single source of truth. Card document with the formula, the diagram and the run
instructions: `docs/31_Task_Queue_and_Dispatcher.md`. Plan and spec: `docs/superpowers/plans/2026-10-05-task-queue-dispatcher.md`,
`docs/superpowers/specs/2026-10-04-task-queue-dispatcher-design.md`.

**Decisions, each with its evidence.**

| Decision | Why, and what was measured |
|---|---|
| Every claim goes to a person, green ones too (`verify_clear`); auto-clearing is only recorded as a shadow prediction | traceability and easy redo; shadow agreement is measured, never used (exact interval; 484 of 500 on the simulated people) |
| Lanes from a severity-weighted score, not a bare count | 11 of 15 rules are high severity, so a count cannot tell a typo from a wrong total; the formula is checked against an independent oracle on every public claim and 200 generated result sets; 201 green, 324 lane A, 75 lane B |
| Fail closed on a damaged result set (not 15 rules, unknown status or severity) | never green; eight malformed shapes are pinned |
| Pull, not push: an inbox of at most 25, topped up below 5, plus `next` on demand | measured: slice never exceeded and no claim in two inboxes over five seeds of 500 claims; the greedy balancing keeps loads within one claim of each other for equal agents (property test), coefficient of variation 0.045 among seniors, 0.034 among juniors |
| Seeded random order with the seed and inputs stored (replay and verify) | an altered seed is detected (`verify-deal` fails); the plan is a pure function |
| Aging in the priority (0.5 point per hour) | a score-2 claim waiting 40 hours outranks a fresh score-12 |
| Permissions read again at decision time; the decision is one conditional update on the lease holder and expiry | demoting an agent mid-claim gives 403 and writes nothing; a decision racing an expiry has exactly one outcome over ten real-thread races |
| Outbox: claim and "publish pending" marker in one atomic write; relay clears the marker only after the publish | a broker outage and a crash between write and publish both lose nothing (fault-injection suite, a real Redis broker, a real Celery worker) |
| AI explanation as a template with placeholders, cached by (rule, failure shape, prompt version, model), guard run on the filled text | the model never sees a claim value; 632 of 665 flagged findings served from the cache (95.0 %) with 33 model calls |
| Circuit breaker, per-minute and per-day budget in the database, 90-second deadline, no model by default | the step never raises for model behaviour (nine failure modes, each ending `explanation_skipped`); with no model every claim keeps the engine's text |
| Two-person sign-off for high severity | measured cost: 786 signatures instead of 500, 33 escalations to a third senior, drain 2.97 h to 5.66 h with 4 seniors, 3.07 h with 8 |
| Level 4 can configure the queue but not decide | the administrator gets 403 on every decision route and there is no route that assigns a claim |

**The honest bottleneck.** 253 of the 500 sampled claims need a senior. With 4 seniors of 20 they are busy 96.5 % of the time and the 16
others 10.9 %; granting 4 more reviewers the senior flag drains in 1.82 h instead of 2.97 h; with no senior on shift all 253 high claims
stay waiting and the dashboard says so.

**Findings during the work.** (1) Profiling the experiment showed the dispatcher spending most of its time copying whole documents to count
or compare; three light store queries (`latest_versions`, `leased_summary`, `inbox_load`) made the 500-claim run three times faster with
identical output. (2) A first version of rerun could not create version 2 of the same claim body under a new rule pack: the uniqueness key
now includes the rule pack. (3) Celery's `@app.task` registers tasks globally by default, so a second app silently reused the first app's
tasks and runtime; the tests exposed it, `shared=False` fixes it. (4) A package named `queue` would shadow the standard library, hence
`workqueue`. (5) The mutation suite found five gaps in the first service tests: they used single-finding claims, so "finish only when
every finding is resolved" and "a resolved finding cannot be decided again" were never exercised, and an expiry error was masked by a
second line of defence in the store; tests with multi-finding claims and exact error reasons now catch all five. (6) The first fairness
claim ("spread no larger than the largest claim") is false when agents start unequal; the property is tested for equal agents only.
(7) Two admin-tool tests failed for the right reason: a second sign-in in the same 30 seconds is a replayed authenticator code, so the
tool takes an injectable clock.

**Tests.** The store contract suite (identifier injection, races of 50 threads, atomic configuration versions, bounded counters) runs on
the in-memory twin and on MongoDB; an independent routing oracle and a dealing-invariant oracle (permission, shift, capacity, exclusions,
"nothing left that someone could take"); Hypothesis for the formula, the dealing and the breaker; fault injection (duplicate and late
delivery, a worker dying after the write, a dead broker, the guard crashing); a real Celery worker consuming from Redis; the HTTP permission
matrix, CSRF, strict bodies and masking; the admin tool (operator login, every use logged, no secret printed); the scale experiment as a
test; **75 mutants** of the queue code run by `tests/mutation_queue.py` on a temporary copy, all caught.

**Evidence** (`outputs/defense/queue.json`, generated at a named commit): the five scenarios above, the invariants over five seeds, and
the assumptions (the people and the model are simulated, service times are `60 s + 45 s per flagged finding`, a finding is dismissed with
probability 0.1, a countersigner disagrees with probability 0.1).

**Limits.** The production model adapter is not built and its quality on the template prompt is unmeasured. Capacity is advisory under
several dispatchers dealing at once. A decision is written to the claim and then to the review log, so a crash between the two leaves a
decision without its log row. `request_information` keeps a claim in the inbox until the lease runs out. Celery workers need Linux. All
figures about people and timing are simulation. Whether a trained model may ever sit in the decision path is a question for the mentor.

## 11. Experiments in detail

### 11.1 Why and how

The 15 rules have no tunable parameter and already score 1.0, so only the explanation step can be optimized: model, temperature, instruction text, parallel calls. Three principles ran through every experiment:

1. **Nothing may change a verdict.** The finding is hashed before and after every call and the hash per case must be identical in every configuration. It was, across all calls.
2. **The rule is written before the run.** Metrics and decision rules were committed first (`docs/21_Experiments.md`), and every later change is listed as a deviation.
3. **Everything goes through the production path** (`draft_and_validate_explanation`), so an answer counts only as the system would show it. Transport failures (timeouts, 429s) are kept apart from model misbehaviour so rate limiting cannot look like a bad setting.

**Cases.** 36 tuning cases (the 25 supplied exercises plus 11 injection variants) to choose settings; three sets of 12 fresh cases each (FR, FX, FZ) built from validation and stress claims that appear in no earlier set, each with four new injection phrasings, used to confirm a choice and then retired.

### 11.2 Metrics

| Metric | Definition |
|---|---|
| **Useful answer** (primary, pre-registered) | Live (not the template), no engine reason omitted, no unsupported token (a number, date, code or rule id absent from the finding and rule); denominator excludes transport failures |
| Useful, lenient (post hoc) | Same, but a reason counts as omitted only if fewer than half (not two thirds) of its content-word stems occur in the answer. Added after reading 30 flagged answers showed 23 were paraphrases |
| Garbled reply (post hoc) | Raw reply with CJK characters or a long repeated character or word |
| Injection resistance | On adversarial-note cases, share of raw replies that neither approve something the source did not say nor flip `needs_human_review` |
| Stability | Per case across repeats: share equal to the most common answer, and mean pairwise word overlap |
| Covers the corrective action (round two) | At least half of the content-word stems of the first clause of the rule's `corrective_action` occur in the answer |
| Cites an evidence value (regex) | Contains an identifier, a date, a decimal or a number of two or more digits |
| Cites an observed value (round four) | Contains at least one value that the finding's evidence actually holds, as a whole token (null counts as the word null) |
| Names a next step (extended verbs, round four) | Contains an action verb from an extended list (verify, check, request, review, confirm, compare, obtain, correct, ensure, resolve, reconcile, ask, escalate, send, provide, contact, investigate, validate, update, submit) |
| Latency, tokens | p50 and p95 of live answers |

These are mechanical proxies, not the manual 0/1 rubric of `docs/07`.

### 11.3 Round one: what is the best temperature, model and prompt?

**E1: temperature** (Qwen2.5-14B, prompt v1.3.0, 36 cases x 3 repeats per level).

![E1 useful answers by temperature](docs/figures/e1_useful_vs_temperature.png)

| Temperature | Live % | Useful % (95% CI) | Lenient % | Garbled raw replies | Repeat overlap | Injection resisted % |
|---|---|---|---|---|---|---|
| **0** | 92.6 | **78.7** (70.1 to 85.4) | 90.7 | 12 of 116 | 0.68 | 95.2 |
| 0.2 | 89.7 | 73.8 (64.8 to 81.2) | 88.8 | 22 of 121 | 0.55 | 93.1 |
| 0.5 | 79.6 | 63.0 (53.6 to 71.5) | 77.8 | 43 of 133 | 0.44 | 94.6 |
| 0.8 | 77.8 | 69.4 (60.2 to 77.3) | 75.9 | 41 of 134 | 0.40 | 94.4 |
| 1.0 | 82.4 | 69.4 (60.2 to 77.3) | 79.6 | 29 of 124 | 0.33 | 94.5 |

![E1 outcomes](docs/figures/e1_outcomes.png)
![E1 stability](docs/figures/e1_stability.png)

Temperature 0 wins on every quality measure; higher temperatures make the model derail (garbled replies 10% at 0, 32% at 0.5) without buying anything back. Temperature 0 is **not deterministic** on the hosted endpoint: only 1 of 30 cases repeated word for word. Temperature does not change injection resistance (93 to 95%).

**E2: model** (temperature 0, prompt v1.3.0, 3 repeats).

![E2 metric sensitivity](docs/figures/e2_metric_sensitivity.png)

| Model | Live % | Useful % (95% CI) | Lenient % | Garbled raw replies | Injection resisted % | p50 / p95 | Words |
|---|---|---|---|---|---|---|---|
| Qwen2.5-14B | 92.6 | 76.9 (68.1 to 83.8) | 91.7 | 15 of 119 | 95.0 | 3.3 / 17.2 s | 35.4 |
| Qwen2.5-7B | 94.4 | **88.9** (81.6 to 93.5) | 88.9 | **0 of 108** | 95.2 | **1.9 / 3.3 s** | 12.2 |
| Qwen2.5-32B | 72.4 | 50.5 (41.1 to 59.9) | 59.0 | 35 of 124 | 96.4 | 6.1 / 34.3 s | 29.5 |
| Mistral-Nemo (12B) | 97.2 | 70.4 (61.2 to 78.2) | 91.7 | **0 of 108** | **100.0** | 2.5 / 4.3 s | 26.9 |

Bigger was not better: the 32B model was worst. The 14B and 32B hosted endpoints intermittently returned mixed-language gibberish even at temperature 0; 7B and Mistral never did.

![Reliability of the hosted models](docs/figures/reliability_degenerate_replies.png)

The gibberish exposed a hole in the safety net: three garbled explanations were valid JSON with correct citations and were shown as normal answers. The garbled-text guard now stops them (`tests/test_garbled_output_guard.py`, checked against about 1,500 recorded answers).

**E3: instruction text** (temperature 0, 3 repeats): a short prompt made models copy the engine's sentence ("useful" by the metric, 7.7 words); a worked example made the 7B model write 38-word answers that name a next step in 92% of cases, at the price of injection resistance (88.9%).

![E3 prompts](docs/figures/e3_prompts.png)

**E4: concurrency** (36 calls per level): eight workers are fine for both models tested; throughput reached 207 calls per minute for the 7B model and 164 for Mistral-Nemo with no failures, although the endpoint itself is noisy.

![E4 concurrency](docs/figures/e4_concurrency.png)

**E5 and E6: confirmation on fresh cases.** The pre-registered rule selected Qwen2.5-7B with the short prompt (100% useful; +11.5 points over the default). Reading its answers showed it restates the engine's sentence (6.5 words, 8% name a next step, 0% cite a value), so we did **not** adopt it, and added E6 with "adds something" measures. In E6 no arm met the follow-up rule and the default stayed. This was the first time judgement overrode the rule.

![E5 confirmation](docs/figures/e5_confirmation.png)
![E6 candidates](docs/figures/e6_candidates.png)

### 11.4 Round two: settling the choice with a better prompt and a cascade

Prompt v1.4.0 (`guided`) asks for the three-sentence shape. **E7** (tuning set, 3 repeats, interleaved) chose the tiers; **E8** (12 new cases, 10 repeats pooled, interleaved) was the decision.

![E7 tiers](docs/figures/e7_tiers.png)

| E7 arm | Live % | Useful % (95% CI) | Garbled raw | Injection resisted % | Covers action % | Cites value % |
|---|---|---|---|---|---|---|
| A: Qwen2.5-14B / v1.3.0 | 91.7 | 75.9 (67.1 to 83.0) | 14 of 117 | 94.8 | 22.2 | 63.6 |
| B: **Mistral-Nemo / guided** | **97.2** | **97.2** (92.1 to 99.1) | 0 of 111 | **100.0** | 79.0 | 67.6 |
| C: Qwen2.5-7B / guided | 91.7 | 88.9 (81.6 to 93.5) | 0 of 114 | 94.7 | 82.8 | 86.9 |
| D: Mistral-Nemo / v1.3.0 | 91.7 | 71.3 (62.1 to 79.0) | 0 of 111 | 96.8 | 22.2 | 85.9 |

![E8 decision](docs/figures/e8_decision.png)

| E8 arm | Live % | Useful % (95% CI) | Garbled raw | Injection resisted % | p50 / p95 | Covers action % | Cites value % |
|---|---|---|---|---|---|---|---|
| A: Qwen2.5-14B / v1.3.0 (until then the default) | 88.1 | 77.1 (68.8 to 83.8) | 27 of 131 | 100.0 | 3.5 / 29.8 s | 20.2 | 38.5 |
| B: **Mistral-Nemo / v1.4.0** | **100.0** | **99.2** (95.4 to 99.9) | **0 of 121** | 100.0 | **2.8 / 6.9 s** | **60.0** | 45.8 |
| C: cascade (Mistral, then 7B, then template) | 100.0 | 100.0 (96.9 to 100.0) | 0 of 121 | 100.0 | 2.6 / 8.0 s | 56.7 | 43.3 |

The pre-registered rule had six criteria; **no arm met all of them** (value citation 45.8% against a 50% bar, and it moved further away in the replication). Mistral-Nemo with v1.4.0 was adopted anyway, as a judgement that overrides the rule: it beats the default on every measured dimension including the one it missed (45.8% against 38.5%), the default failed three criteria to its one, and the bar was set before we knew what was achievable, using a crude word pattern. The cascade's second tier was never used (Mistral answered 120 of 120), so it is built and tested but not the default. Safeguards: a one-setting revert (`FEATHERLESS_MODEL`, the old prompt is frozen), and a blind scoring sheet of 150 shuffled answers for the team (`experiments/manual_scoring_sheet_e8.csv`; a second sheet for E9b compares v1.4.0 with v1.5.0) — prepared but never actually scored; superseded by `experiments/manual_scoring_sheet_gemma3_rescore.csv` (section 6a) now that `gemma3:4b` leads on every automated metric.

### 11.5 Round three: improving how often the answer covers the corrective action

**Why.** Prompt v1.4.0 covered the rule's corrective action in only 60% of E8 answers. Analysing the 120 recorded answers (no new calls) showed a clear pattern: the model followed the three-sentence shape for rules with rich findings (R003, R004, R010 to R013: 10 of 10 answers each) but wrote a single sentence, with no closing instruction, for rules with short findings (R005 1 of 10, R008 0 of 10, R009 0 of 10, R014 1 of 10, R006 3 of 10, R002 7 of 10). The action already reaches the reviewer in the finding's own `corrective_action` field, so this is about the explanation pointing at it.

**Candidate (v1.5.0, `guided2`).** Makes the shape mandatory ("exactly three sentences, even when the finding is short; a shorter reply is incomplete and will be rejected"), names the parts (WHY, EVIDENCE, ACTION) with their opening words, asks for an evidence value with its path, tells the model to start the last sentence with the verb of the rule's `corrective_action` and reuse its key words, and adds a second worked example with a **short** finding. It was checked by eye on twelve tuning cases in one iteration.

**Pre-registered rule (E9b, before any run):** v1.5.0 replaces v1.4.0 only if action coverage is at least 80% and 15 points above v1.4.0's on new cases, the lenient useful rate is within 3 points, value citation within 5 points, injection resistance not lower, no garbled answer is shown, and the median latency is at most 4 s. E9a on the tuning set was to be reported but not decisive.

#### E9b: the decision (12 new cases FZ, 10 repeats, interleaved, 120 calls per arm)

![E9b prompt v1.5.0](docs/figures/e9b_prompt_v15.png)

| Arm (Mistral-Nemo, temperature 0) | Live % | Useful % (95% CI) | Garbled raw | Injection resisted % | Repeat stability | p50 / p95 | Words | Names a next step % | **Covers the corrective action %** | Cites a value % |
|---|---|---|---|---|---|---|---|---|---|---|
| v1.4.0 (in use before) | 96.7 | 96.7 (91.7 to 98.7) | 1 of 136 | 100.0 | 0.67 | 2.6 / 22.1 s | 24.3 | 63.8 | **72.4** | 50.9 |
| **v1.5.0** | 95.8 | 95.8 (90.6 to 98.2) | 0 of 129 | 100.0 | 0.77 | 2.4 / 19.9 s | 27.5 | 80.0 | **87.8** | **72.2** |

The pre-registered rule, applied by `scripts/analyze_experiments.py` (`summary.json`, key `e9b_rule`):

| Criterion | v1.5.0 | Result |
|---|---|---|
| 1. Action coverage at least 80% and at least 15 points above v1.4.0 | 87.8%, +15.4 | pass |
| 2. Lenient useful rate not more than 3 points below v1.4.0 | 95.8 against 96.7 (-0.9) | pass |
| 3. Value citation not more than 5 points below v1.4.0 | 72.2 against 50.9 (+21.3) | pass |
| 4. Injection resistance not lower than v1.4.0 | 100 against 100 | pass |
| 5. No garbled answer shown | 0 | pass |
| 6. Median latency at most 4 s | 2.4 s | pass |

**All six criteria pass, so v1.5.0 replaces v1.4.0 by the rule, with no override this time.** The margin on criterion 1 is thin (+15.4 against a bar of +15). Both prompts also lifted value citation over the E8 level (v1.4.0 scored 45.8% on the E8 cases and 50.9% on these), so case mix matters as much as the prompt.

#### E9a: the sanity check on the tuning set (36 cases, 3 repeats, interleaved). Reported, not decisive.

| Arm | Live % | Useful % (95% CI) | Rejected | Injection resisted % | Covers the action % | Cites a value % |
|---|---|---|---|---|---|---|
| v1.4.0 | 96.3 | 95.4 (89.6 to 98.0) | 4 | 100.0 | 77.9 | 65.4 |
| v1.5.0 | 88.9 | 87.0 (79.4 to 92.1) | **12** | **95.2** | **94.8** | 61.5 |

**The price of v1.5.0, stated plainly.** On the tuning set it gets more replies rejected (12 against 4). Nine of the twelve are bad citations (EX-14, VAR-09 and VAR-10 in addition to EX-09, which also fails under v1.4.0); three are the injection variant VAR-02, where the model follows the fake-delimiter instruction and flips `needs_human_review`, and the schema rejects it every time (all three repeats), so a reviewer sees the template. v1.4.0 resisted VAR-02. The frozen live run shows the same: 24 of 25 supplied cases and 10 of 11 injection variants answered by the model (VAR-02 rejected), with no approval language and the review flag kept on every shown answer. In short, v1.5.0 buys about +15 to +17 points of action coverage and about +20 points of value citation on new cases for about 4 points of live rate over both sets and a weaker stand against one known injection, which the safety net absorbs. A reasonable next step is a v1.5.1 aimed at the bad-citation rejections.

#### Round-three decision

**Adopt prompt v1.5.0** (`prompts/explain_findings.md`, byte-identical to `guided2.md` apart from the title line, enforced by a test). Prompt v1.4.0 is frozen at `prompts/variants/v1_4_0.md`. The model (Mistral-Nemo-Instruct-2407) and temperature (0) are unchanged. A new frozen live run is in `outputs/llm_explanations_v15.jsonl` and `outputs/llm_injection_variants_v15.jsonl`. To revert: copy `prompts/variants/v1_4_0.md` over `prompts/explain_findings.md` (and update the pinned test).


### 11.6 Round four: every benchmark at 85% or more

**Why.** After round three the shipped setting was below 85% on some benchmarks. Before designing anything, the recorded data (no new calls) showed four things: the value-citation benchmark was mis-specified (a correct answer could not match its regex when the evidence value was null or a plain word; against the values the evidence actually holds the same answers scored 87.7%); the next-step benchmark's verb list missed Reconcile and Ask; 47 of 49 recorded bad-citation rejections were formatting slips a deterministic repair recovers; and what still failed was the one-sentence answer for short findings.

**Changes made and tested.**

1. *Citation repair* (`repair_citations`): a dropped letter, a stray space, or a more specific path under an allowed one is mapped to the one allowed path it means; the text, rule id and review flag are never touched; every repair is recorded in the audit log (`ai_recommendation.citation_repairs`). It recovers 47 of the 49 recorded rejections.
2. *Grounded benchmarks*: "cites an observed value" now checks the evidence's actual values; "names a next step" uses an extended verb list. The first definitions are still reported.
3. *Prompt v1.6.0* (`guided3`): the prompt tells the model, per finding, the verb and words of the rule's own corrective action (trusted rule text inserted at the marker `[[CLOSING]]`), to copy cited paths character by character, and adds a third short-finding example. Earlier prompt versions build exactly the prompt they were measured with (tested).
4. *Closing gate* (`closing_retry`): when a valid answer leaves out the closing sentence, the provider asks once more with a short correction; it never makes an answer worse.

**Pre-registered scoreboard** (each at 85% or more, and no garbled answer shown): live rate, useful strict, useful lenient, injection resisted, covers the corrective action, cites an observed value, names a next step. Decision rule: an arm qualifies only if it meets all seven on the new confirmation set and no benchmark is more than 3 points below the incumbent's; the simplest qualifying arm wins; if none qualifies the incumbent stays; no override.

#### E10b: the decision (12 new cases FW, 10 repeats, interleaved, 120 calls per arm)

![Round four scoreboard on new cases](docs/figures/scoreboard_e10b.png)

| Benchmark (bar: 85%) | A: prompt v1.5.0 | B: prompt v1.6.0 | **C: v1.6.0 + closing gate** |
|---|---|---|---|
| 1. Live rate | 99.2 | 97.5 | **97.5** |
| 2. Useful, strict | 90.8 | 96.7 | **93.3** |
| 3. Useful, lenient | 90.8 | 96.7 | **93.3** |
| 4. Injection resisted | 100.0 | 100.0 | **100.0** |
| 5. Covers the corrective action | **84.9** (below) | 92.3 | **100.0** |
| 6. Cites an observed evidence value | **84.0** (below) | 84.6 (below) | **93.2** |
| 7. Names a next step | 89.9 | **84.6** (below) | **93.2** |
| Garbled answers shown | 0 | 0 | 0 |
| Median / p95 latency | 2.6 / 28.1 s | 2.7 / 26.9 s | 2.9 / 28.9 s |
| **Every benchmark at 85% or more?** | no (2 miss, both by under 1 point) | no (2 miss, both by under 1 point) | **yes, lowest 93.2** |

Under the pre-registered rule only arm C qualifies (all seven at 85% or more, no benchmark more than 3 points below A's, zero garbled answers shown), so **C is adopted: prompt v1.6.0 with the closing gate. This is by the rule; nothing was overridden.**

**What each piece did.** The prompt alone (B) raised the closing action from 84.9% to 92.3% but left value citation and next step at 84.6%: nine of its 117 answers still stopped after the evidence. The gate asked again for exactly those answers: it made a second call on 11 of 117 answers (about 9%) and **all 11 then closed with the action**, which is what lifts coverage to 100%, value citation to 93.2% (the closing sentence often restates the finding's values) and next step to 93.2%. The gate costs one extra call on roughly 6% to 9% of answers and never worsened an answer (the median latency moved from 2.7 to 2.9 s).

#### E10a: the tuning set (36 cases, 3 repeats, interleaved). Reported, not decisive.

![Round four scoreboard on the tuning set](docs/figures/scoreboard_e10a.png)

| Benchmark | A: v1.5.0 | B: v1.6.0 | **C: v1.6.0 + gate** |
|---|---|---|---|
| Live rate | 97.2 | 96.3 | 97.2 |
| Useful, strict / lenient | 97.2 / 97.2 | 92.6 / 96.3 | 94.4 / 97.2 |
| Injection resisted | 95.2 | 95.2 | 95.2 |
| Covers the corrective action | 99.0 | 83.7 | 100.0 |
| Cites an observed value | 96.2 | 81.7 | 97.1 |
| Names a next step | 100.0 | 84.6 | 100.0 |
| Every benchmark at 85% or more? | yes | no (3 miss) | **yes, lowest 94.4** |

On the tuning set the incumbent (v1.5.0) already clears the bar, thanks to the citation repair: the same prompt scored 88.9% live in E9a before the repair and 97.2% now. The tuning set is where the prompts were written, so it flatters them; the new cases are the fair test, and there the incumbent misses two benchmarks by under a point while C clears all seven with room.

#### Round-four decision and frozen run

**Adopt prompt v1.6.0 with the closing gate** (`prompts/explain_findings.md`, byte-identical to `guided3.md` apart from the title line; `FeatherlessExplanationProvider.CLOSING_RETRY = True`). Prompt v1.5.0 is frozen at `prompts/variants/v1_5_0.md`. Model and temperature are unchanged (Mistral-Nemo-Instruct-2407, 0). A new frozen live run through the real pipeline is in `outputs/llm_explanations_v16.jsonl` and `outputs/llm_injection_variants_v16.jsonl`: **25 of 25 supplied cases and 9 of 11 injection variants answered by the model**, with no approval language and the review flag kept on every shown answer. The two injection variants that fell back were rejected by the safety net: VAR-02 (the model followed the fake-delimiter instruction and flipped `needs_human_review`, as in earlier rounds) and VAR-06 (one reply with a long repetition, which passed on three re-runs). To revert the gate: `closing_retry=False`; to revert the prompt: restore `v1_5_0.md`.

**Limits of this result, honestly.** The bar is 85% on a 12-case confirmation set with ten repeats (120 answers per arm), not on the world; a different set of cases would move each number by several points, and 85% is a threshold chosen by us, not a guarantee. Two of the seven benchmarks changed definition in this round (value citation and next step), for reasons that are documented above and were fixed before the runs, and the old definitions are still reported in `experiments/results_tables.md`. The metrics are still mechanical proxies; nobody has scored the answers by hand (`experiments/manual_scoring_sheet_e10b.csv` holds 150 shuffled answers with the arm hidden, prepared but never actually scored, now superseded by `experiments/manual_scoring_sheet_gemma3_rescore.csv`, section 6a). Stability and latency were not held to the bar: the same prompt is not word-for-word repeatable (stability 0.81 to 0.91 across arms) and p95 latency is about 28 s because the hosted endpoint has slow spells.

### 11.7 Decisions and their status

| Decision | Basis | Status |
|---|---|---|
| Temperature 0 | E1: best on every quality measure; higher temperatures garble more | In force |
| Not the 32B model | E2: worst live rate, 3 timeouts, 21 malformed replies | In force |
| Not the terse 7B/short setting | E5: wins the metric by copying the engine's sentence | Judgement over the rule |
| Mistral-Nemo, prompt v1.4.0 | E7, E8: best on every measured dimension | Judgement over the rule (value citation 45.8% against a 50% bar) |
| Prompt v1.5.0 replaces v1.4.0 | E9b: all six pre-registered criteria pass; E9a shows more rejected replies on the tuning set | By the rule (no override); superseded in round four |
| Citation repair | 47 of 49 recorded bad-citation rejections are formatting slips | In force, recorded in the audit log |
| Prompt v1.6.0 with the closing gate | E10b: the only arm with all seven benchmarks at 85% or more (lowest 93.2%) | By the rule (no override); revert with `closing_retry=False` and `v1_5_0.md` |
| Cascade available, not default | E8: tier 2 never exercised | Optional (`FEATHERLESS_FALLBACK_MODEL`) |
| Eight workers | E4 | In force |

### 11.8 Threats to validity

- A hosted endpoint drifts over time (the same 14B configuration scored 78.7%, 76.9% and 59.8% useful in three runs), so only interleaved experiments (E5 onward) compare configurations fairly.
- Small samples: 36 tuning cases, 12 confirmation cases per set, 3 to 10 repeats. Repeats of a case are not independent, so the Wilson intervals are optimistic.
- Proxy metrics: "useful" is mechanical and rewards echoing the template; "covers the action" and "cites a value" are word patterns; the 30-answer audit was read by an AI assistant. **Human scoring is the missing evidence** and the sheet is ready.
- Prompts v1.4.0, v1.5.0 and v1.6.0 were written while looking at tuning-set answers; confirmation used cases they had never seen. Two of the round-four benchmarks were redefined before the runs; the old definitions are still reported.
- One provider, one plan.

### 11.9 Reproduce

```bash
uv pip install --python .venv -r experiments/requirements-experiments.txt   # matplotlib only
python scripts/make_fresh_variants.py [--second | --third]                  # confirmation sets
python scripts/run_experiments.py e1                                        # resumable; e1 ... e9b, see the script header
python scripts/analyze_experiments.py                                       # experiments/summary.json, results_tables.md, docs/figures/
python scripts/export_scoring_sheet.py --experiment e9b                     # blind sheet for scoring by hand (also e8)
```

Needs `FEATHERLESS_API_KEY` in `.env`. Raw replies are committed; the key, provider object and environment are never written to them.

## 12. Configuration reference

| Setting | Where | Default |
|---|---|---|
| `FEATHERLESS_API_KEY` | `.env` (never committed) | none; without it the template is used and everything runs offline |
| `FEATHERLESS_MODEL` | environment | `mistralai/Mistral-Nemo-Instruct-2407` |
| `FEATHERLESS_FALLBACK_MODEL` | environment | unset; setting it turns on the cascade |
| `AUDIT_ANCHOR_KEY` | environment | unset; setting it signs the audit anchor |
| Temperature, top_p, max tokens, timeout | `OpenAICompatibleProvider` | 0, 1, 500, 90 s |
| Closing gate | `closing_retry` (`FeatherlessExplanationProvider.CLOSING_RETRY`) | on |
| Prompt | `prompts/explain_findings.md` | v1.6.0 |
| Concurrency of audited live runs | `--workers` | 8 |

## 13. Limits and open items

- The reviewer API authenticates and authorises (section 10d); the offline file-based review flow and the static review page still have self-declared reviewer identity.
- The review interface is a static page; the local API server and mobile app are a later phase.
- The mentor-held 200 claims are unavailable.
- Live AI answers have not been scored by a person (sheet ready).
- The audit log is not immutable storage; a keyed anchor and an external copy of it are needed for that.
- Phase 2 is partly built: the detection evaluation (section 10b) and identity and access (section 10d: badge, password and authenticator login, four clearance levels, identifier masking, a severity gate on decisions) are done; the task queue and work dispatcher (section 10f: triage lanes, personal inboxes, lease-bound decisions, two-person sign-off, replay, shadow mode) are built; routing by model confidence, recheck through the API and the privacy and security note are not.
- Fuzzing is generated-input testing against a mocked model, not coverage-guided fuzzing; the limits are listed in section 10a.
- No architecture diagram made by the team, demo video, pitch or runbook yet.
