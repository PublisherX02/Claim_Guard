# 24 | Two architectures for claim pre-validation: rules first, or an agent first

The mentor asked us to compare our design with a different way of solving the same problem. Both architectures take a healthcare claim and tell a
reviewer what is wrong with it before a person looks at it. They differ in **who decides**: in one, fixed rules decide and a language model only
explains; in the other, a language model agent decides. This document explains each architecture, then how we compared them, then what we found.

Method and design: the comparison design spec under `docs/superpowers/specs/`. To reproduce: `comparison/README.md`.
The model screening behind the second run is in `docs/25_Comparison_Model_Selection.md`.

## The two architectures at a glance

| | Architecture A: rules decide, the AI explains | Architecture B: an agent decides |
|---|---|---|
| Who decides a claim's status | 15 deterministic rules | A language model agent, from one large prompt |
| What the model does | Writes a plain-language explanation of a finding the rules already made | Retrieves policy text, reasons over the claim, and produces the findings itself |
| Can the model change a verdict | No. The reply is rejected if it tries | Yes. The model's output is the verdict |
| Reads scanned documents | No (structured claims only) | Yes (OCR on images and scanned PDFs) |
| Audit trail | Hash-chained, tamper-evident log of every step | None |
| Output checking | Schema, evidence-citation and grounding checks, with a safe fallback | None; the reply is used as written |
| Built as | Our project | A separate prototype by a teammate |

## Architecture A: rules decide, the AI explains

**What it does.** It reads a claim, runs it through 15 fixed rules, reports every problem with the exact evidence and a next step, and asks a
language model only to explain each problem in plain words. A person always makes the final decision.

**How it works.**
1. *Ingestion.* Claims arrive as FHIR bundles, CSV folders or JSONL and are normalized into one format. A damaged record is quarantined with a reason and never stops the batch.
2. *Facts.* One function per rule reads the claim and produces short factual statements, with claim data encoded so a value cannot forge a statement.
3. *Rules.* A rule pack written in YARA-X, a declarative pattern language, matches those statements and gives each rule a status: PASS, FAIL, UNABLE_TO_ASSESS or NOT_APPLICABLE. A crashing rule becomes UNABLE_TO_ASSESS for that rule only. Missing data is never a pass.
4. *Explanation.* For each FAIL or UNABLE_TO_ASSESS finding, a model writes a short explanation. The reply must match a strict schema, cite only evidence that exists, keep the review flag, and pass grounding checks. If it fails any check, the engine's own sentence is used instead.
5. *Audit.* Every check, every model question (written before the model is called) and every decision goes into a hash chain that is checked against a separate anchor.
6. *Review.* A person confirms, dismisses with a reason, or requests information; a correction is rechecked as a new run and the original is never edited.

**What it costs.** Every rule has to be written and tested by hand, and it only handles the kinds of claim its rules cover. It has no way to read a scanned document or answer a free-form question.

## Architecture B: an agent decides

**What it does.** It takes a claim document (a PDF, image, spreadsheet or text), extracts the claim, and lets a language model agent check it against
policy text, decide which rules are broken, rate severity and confidence, and recommend next steps.

**How it works.**
1. *Extraction.* OCR (Tesseract) turns images and scanned PDFs into text, and a model call turns that text into a structured claim.
2. *Retrieval.* The payer policy text is split into chunks and turned into vectors, stored in a FAISS index. The agent can search it by meaning.
3. *Agent.* A ReAct-style agent (built with LangGraph) runs a loop: it calls a `retrieve_documents` tool to fetch relevant policy text and a `calculator` tool for arithmetic, then writes the result. One large system prompt tells it how to judge a claim and what shape to answer in.
4. *Conversation.* The agent keeps the conversation, so a reviewer can ask follow-up questions about a claim.
5. *Guardrail.* A small pattern list blocks clearly unsafe requests.

**What it costs.** Correctness depends on the model following a long prompt, producing well-formed output and using tools reliably. There is no schema check on the answer, no audit log, no tests and no continuous integration, and the server component is an empty file. It is early-stage work, which is a fair state for that stage; it is not a criticism of the person who built it.

**What it does that A does not.** OCR for scans and images, a multi-turn question-and-answer interface over a claim, and a general retrieval layer that can take arbitrary policy documents without new code. Our claim sample was already structured, so OCR was not exercised in this comparison.

## How we compared them

- **Same model on both.** Otherwise we would be measuring the model, not the architecture. Architecture B's own code was changed in one place only: which model it calls.
- **Real policy text for B.** B's policy folder was given a full prose version of all 15 rules, so its retrieval had real content to find.
- **Scoring by formula, from recorded runs.** The comparison scoring script in `scripts/` is deterministic; no number is typed in by hand. Weights: correctness 25, hallucination 15, security 20, deliverability 15, rapidness 15, efficiency 10. A system is scored on speed only for claims it actually answered, so failing fast is not mistaken for being fast.
- **Hallucination.** Two rates over the findings a system produced, averaged, where 100 means none: fabricated findings (a FAIL on a rule the answer key says did not fail, or a rule id outside R001 to R015) and ungrounded evidence (a cited evidence value that appears nowhere in the claim). A system with no parseable findings scores 0, since no answer is not an honest answer.
- **Two readings of B's output.** B has no output parser, and a model often wraps valid JSON in prose or a code fence. *Strict* counts only a reply that is JSON from its first character. *Lenient* takes the first JSON object found in the text. Lenient is a disclosed adjustment for a formatting habit, not a change to B's logic. Both are reported.
- **Same security scan on both codebases:** dangerous-call scan, presence of a citation-grounding check, and one prompt-injection probe.

## Experience 1: a small local model (gemma3:4b, offline): B could not run

Architecture B is built on tool calling, and `gemma3:4b` does not support it: every one of 36 claims failed at the model call with "does not support tools".
The 96.67 to 13.33 score from that run measured the model, not the architecture, and is **not** a result to cite as a win for A. What it does show:
a design that depends on a model capability such as tool calling is more fragile to the choice of a local model than one that only asks a model to
explain text. Evidence: `outputs/architecture_comparison/gemma3-4b-ollama/`; figures `docs/figures/architecture_comparison_*_gemma3.png`.

## Experience 2: a tool-capable hosted model (Qwen2.5-14B-Instruct): both run

- **Model.** `Qwen/Qwen2.5-14B-Instruct` on both. Our production explanation model is Mistral-Nemo, but it writes tool calls as plain text and cannot drive Architecture B, so the shared model had to differ from it. Screening table: `docs/25_Comparison_Model_Selection.md`.
- **Hosted, not offline.** This run needs the network; the fully-local property belongs to Experience 1 only.
- **Sample.** The first 12 claims of the development split (kept small on purpose). The first 6 were also in the model screening.
- **Order.** B, then A, then the security probe, one after another after a warm-up call. A's explanation step was real: 16 explanations, none replaced by the fallback.

### The limit that matters most: the hosted endpoint degenerated on B's runs

Of B's 12 replies, **8 were degenerate** (long runs of `!!!!!!…`, or a tool call written as text instead of executed) and **2 were service errors**.
Only 2 to 3 of 12 held a parseable answer. The same model on the earlier 6-claim screening gave 6 of 6 lenient answers with no degenerate output, so
the failure rate moved between runs. A's calls are short (about 85 completion tokens); B's agent produces long outputs across several tool rounds, so
it is more exposed to an endpoint that degrades. We cannot separate "the endpoint glitched" from "this design needs long stable generations". Read the
numbers below as **what B did on this endpoint on this run**, not as its best case. A re-run on a stable endpoint, or on local tool-capable serving, is
the way to settle it.

### Results

| Category (weight) | Architecture A | Architecture B, strict | Architecture B, lenient |
|---|---|---|---|
| Correctness (25) | 100.0 | 0.0 | 8.3 |
| Hallucination (15) | 100.0 | 0.0 (no answers) | 66.0 |
| Security (20) | 100.0 | 35.0 | 20.0 |
| Deliverability (15) | 83.33 | 16.67 | 16.67 |
| Rapidness (15) | 100.0 | 0.0 | 4.0 |
| Efficiency (10) | 100.0 | 20.0 | 20.0 |
| **Overall** | **97.5** | **11.5** | **21.08** |

Figures: `docs/figures/architecture_comparison_{categories,overall}_qwen25-{strict,lenient}.png`. Raw rows and verdicts are in
`outputs/architecture_comparison/qwen25-14b-featherless/`.

**Correctness.** A matched the answer key on 12 of 12. That is guaranteed by construction (CI already demands a perfect score on this split), so it is not
an independent finding. B matched 0 of 12 strictly and 1 of 12 leniently.

**Hallucination.**

| | Claims answered | Findings | Fabricated | Evidence values | Ungrounded |
|---|---|---|---|---|---|
| Architecture A | 12 | 180 | 0 | 570 | 0 |
| Architecture B (lenient) | 4 | 9 | **5** | 16 | 2 |

A's zeros are structural: its FAIL statuses come from the rules, which match the key, and its evidence is a direct lookup in the claim. B's 5 fabricated
findings out of 9 is a real measurement on a very small base (4 claims, 9 findings), so the 66.0 has a wide margin and should not be quoted as a rate.

**Security.** Dangerous-call scan: A has none; B has two (a calculator that uses `eval` on a pre-checked expression, and a bare `input()` call). A has a
citation-grounding check; B has none. **The injection probe found something real:** a claim with a genuine currency violation (R015) and an injected
instruction in its notes came back from B as `overall_status: VALID`, "The claim is fully valid with no findings." The reply was wrapped in prose, so strict
scoring recorded it as "not verified" (-15), while lenient scoring reads it as an incorrectly valid claim (-30), which is why the lenient security score is
lower. It is one probe, one model, one run. A was not sent the probe because it has no free-text conversational surface for it; its resistance to injection is
covered by its own tests (`tests/test_stress_ai_boundary.py`, `docs/20`), and its score skips that one deduction tier.

**Rapidness.** A: median 3.6 s per claim including live explanations. B: only 3 claims were answered even leniently, at 14 s, 46 s and 116 s (median 46 s);
with n=3 that is an anecdote, not a rate. Strictly, B answered none, so its strict rapidness is 0. (The hallucination table counts 4 claims because one more
reply held parseable findings without an overall status.)

**Deliverability and efficiency.** A checklist (audit log, test suite, CI, authentication design, offline capability, schema-validated output): A has 5 of 6,
B has 1 of 6; authentication design is missing for both. Third-party runtime dependencies: 3 for A and 15 for B, normalised to the leanest.

## Verdict

On this run Architecture A scored 97.5 and Architecture B 11.5 (strict) or 21.1 (lenient), and A led every category under both readings. Read that with
the limits above: 10 of B's 12 runs hit an endpoint failure, the sample is 12 claims, the hallucination base for B is 4 claims, A's correctness and
hallucination scores are structural, and the injection probe is a single trial.

What the data supports without those caveats:
1. A design whose correctness depends on a model producing a long, well-formed, tool-using generation is more exposed to model and endpoint variability than
   one that uses the model only to explain a verdict the rules already made.
2. In the one injection probe we ran, the agent design let injected text change the verdict, and the rules-first design has no such path.
3. A's checks cost more to build, but every claim got the same, answer-key-matching result.

What it does not support: a claim that Architecture B's reasoning is worse in general, or that an agent approach has nothing to offer; its OCR and
question-answering are real capabilities A lacks. A stable, tool-capable, locally served model would be the fair re-test, and is the natural next step
given the mentor's on-premises requirement.

## Notes added after the recorded runs (2026-10-04)

- **Calculator guard.** Architecture B's calculator tool accepted exponent chains such as `9**9**9`; a test run was stopped after five seconds without finishing. A
  small guard (`comparison/architecture_b/calc_guard.py`) now rejects `**` and expressions over 200 characters before
  evaluation. Ordinary arithmetic is unchanged and the built-in evaluator is still called behind the guard, so the recorded
  security scan, which counted two dangerous sinks in B, still describes the code. The comparison runs were not repeated after this change.
- **Injection probe reading.** The recorded probe reply was `VALID` wrapped in prose, so strict parsing marked it
  unparseable and left the verdict flags `null`. The scan now also records the lenient reading, the one the harness already
  uses for the lenient score, beside the strict fields and never instead of them. For the saved Qwen2.5-14B report this
  adds `wrapped_json: true`, `lenient_status: "VALID"` and `lenient_marked_valid: true`, recomputed offline from the stored
  raw reply with `python scripts/security_scan_architecture_b.py --reclassify <report>`. No model was re-run and no score
  changed; the strict fields are as recorded.
