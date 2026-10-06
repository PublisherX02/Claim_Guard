# 20 | Security audit

Audited 2026-09-26 against the **OWASP Top 10 for LLM Applications (2025)** and the **OWASP Top 10 (2021)**. Scope: this repository (rule engine, ingestion, bounded AI step, audit log, offline review page). Synthetic data only; there is no server, no database and no user account system.

Note on the standard: ISO does not publish a "top 10". The two OWASP lists are the closest match to "AI usage and cyber threats". ISO/IEC 42001 (AI management) and ISO/IEC 27001 (information security) are management-system standards; a control mapping to them is a separate exercise.

## How it was checked

| Method | Result |
|---|---|
| `bandit` over `src/` and `scripts/` | 0 high. 14 low (`assert` in the organizers' `validate_pack.py`). 1 medium, a false positive (a dict named `requests` in `audit_log.py`). |
| `pip-audit` on the pinned `requirements.txt` | No known vulnerabilities. |
| `detect-secrets` over every tracked file | Only SHA-256 digests and base64 of synthetic FHIR documents. |
| History scan | The key in the untracked `.env` appears in no commit and no tracked file. |
| Manual review of every place claim data crosses a boundary | Rule facts, model prompt, model reply, HTML page, JSONL files, audit log. Findings below. |
| Adversarial tests | `tests/test_security_owasp.py`, plus the earlier `test_stress_*` suites. All run offline in the normal suite (410 tests). |

## Findings fixed in this audit

| # | Finding | Severity | Fix and test |
|---|---|---|---|
| F1 | **Rule-fact injection (A03, LLM01-adjacent).** Claim values (currency, provider id, member id, coverage status, modifier) were copied into the text blob that the YARA pack matches by substring. A value such as `USD R009:MISMATCH:` forged a FAIL on another rule, or a fake UNKNOWN that turned a PASS into UNABLE. A forged tag can only escalate a verdict, never hide a real FAIL (FAIL has top precedence), but it produced false findings with contradicting evidence. | Medium | Claim data is percent-encoded before it enters a fact (`facts_extractor._q`), which removes the colon and newline every tag needs. `RuleFactInjection` puts every tag from `core.yar` into every string field of 40 claims and requires the verdicts to equal the independent oracle. It fails on the previous code. |
| F2 | **Unbounded prompt size (LLM10).** A claim value or document note of any size went into the model prompt. | Low to medium | Values are cut at 1,000 characters, the untrusted note at 4,000, and a prompt over 60,000 characters fails closed to the deterministic template. Tests in `LLM10_UnboundedConsumption`. |
| F3 | **Log forging (A09).** A claim id or a model error containing line breaks could write fake log lines. | Low | Both log calls print values with `%r`. `A09_LoggingFailures`. |
| F5 | **Replayed claims went unnoticed (red team).** The same claim_id submitted three times produced three independent runs with no warning, so a defect could be quietly fixed and resubmitted under the same id. | Low to medium | Every resubmission outside the recheck flow writes a `duplicate_submission` event (earlier run ids, identical or CHANGED content) in the same locked append as the run start, and the claim is routed to a human even if it is clean. A reviewer-requested recheck is not reported as a duplicate. `tests/test_redteam_findings.py`, including six simultaneous submissions. |
| F6 | **Defects that none of the 15 rules cover passed cleanly (red team).** A diagnosis code that is not in `rules/diagnoses.json`, or a payer that differs from the policy's, gave 15 PASS. | Low (scope) | `src/advisory.py` records `advisory_check` events and routes the claim to a human. They are not rule results: the scored output, the 9,000 public results and the rule statuses do not change (tested), and no public claim triggers one. |
| F4 | **Forgeable audit anchor (A02, A08).** Whoever can write the log can rewrite the whole hash chain and its anchor and still verify. | Medium for the "immutable" claim | Optional `AUDIT_ANCHOR_KEY`: the anchor carries an HMAC-SHA256 of head and count, and verification requires it. `A02_A08_TamperEvidence` shows the same forgery passing without a key and failing with one. The default is still unkeyed, and the test that documents this limitation stays. `scripts/verify_audit.py` now reports whether the anchor is signed and whether a key is configured, prints a warning for an unsigned anchor instead of a bare "Chain OK", and `--require-key` makes an unsigned or unverified anchor a failure. |

Fixed in the earlier stress pass and relevant here: the AI trust boundary now runs in the orchestrator for every provider (LLM05, LLM06); a provider can no longer edit the finding it explains; U+2028 in a reviewer's reason no longer breaks the audit log; the runner and ingestion quarantine hostile input instead of aborting (A04, A05).

## OWASP Top 10 for LLM Applications (2025)

| ID | Risk | Status | Evidence and residual risk |
|---|---|---|---|
| LLM01 | Prompt injection | **Mitigated, residual** | Claim text and attachment text are data: labelled untrusted, fenced, and the reply must fit a per-finding schema whose citations, rule id and review flag are fixed by the finding. 25 supplied and 11 own injection cases were run live; none changed a finding. One live reply obeyed a fake delimiter and was rejected by the schema. A model can still write a misleading explanation that passes the schema; the grounding guard catches only known patterns. Update 2026-10-06: replies containing invisible characters (zero-width, bidirectional, soft hyphen) are now rejected, which closes the zero-width word-splitting case (`docs/32`, section 5); non-English Latin text and Base64 remain unguarded. |
| LLM02 | Sensitive information disclosure | **Partial** | The prompt carries the finding and its evidence, not the whole claim, but evidence can include member ids and document text. Data is synthetic. With real data this sends PHI to a third-party API (Featherless.ai); that would need a data-processing agreement or an on-premise model. No key or secret ever enters a prompt. |
| LLM03 | Supply chain | **Partial** | Dependencies pinned to exact versions (tested); `pip-audit` clean. Hashes are not pinned (`--require-hashes`), and the open-weight model behind the provider is not verified. |
| LLM04 | Data and model poisoning | **Not applicable / low** | Nothing is trained or fine-tuned and there is no retrieval index. The rulebook files are covered by the release checksums, and every audit run records the rule pack hash and now an engine code hash. |
| LLM05 | Improper output handling | **Mitigated** | Every model reply is validated (schema, citations, grounding, garbled text) in the orchestrator before use, otherwise the template is used. Two later changes were reviewed against this row: the citation repair only maps a formatting slip in a cited path to the one allowed path it means (text, rule id and review flag are never touched, unmappable paths are still rejected, every repair is logged), and the closing gate re-asks with a hint built only from trusted rulebook text and can only keep or improve a valid answer. The review page writes text only (`textContent`), never markup; hostile strings are tested. |
| LLM06 | Excessive agency | **Mitigated** | The model has no tools, no write access and no way to change a result. Its actions are logged as `human_escalation`, and the audit log rejects any auto-correct action. A provider that edits the finding it is handed changes nothing (it receives copies). |
| LLM07 | System prompt leakage | **Low** | The prompt (`prompts/explain_findings.md`) is public in the repo and holds no secret or credential. |
| LLM08 | Vector and embedding weaknesses | **Not applicable** | No embeddings or vector store. |
| LLM09 | Misinformation | **Partial** | Human review is mandatory for every FAIL and UNABLE. A guard rejects invented currency symbols, relative-time claims and unsupported validity statements. The manual 0/1 scoring of live answers is not filled in. |
| LLM10 | Unbounded consumption | **Mitigated, residual** | Request timeout, `max_tokens=500`, one retry for transient failures plus at most one closing-gate call (so at most three calls per answer, and the gate fires on roughly 6% to 9% of answers), bounded prompts (F2), 8 concurrent calls. There is no overall budget per run, so a very large file could make many paid calls. |

## OWASP Top 10 (2021)

| ID | Risk | Status | Evidence and residual risk |
|---|---|---|---|
| A01 | Broken access control | **Gap (by scope)** | No accounts, roles or authentication. The review page and decision files are local. Any local user can write decisions. |
| A02 | Cryptographic failures | **Partial** | SHA-256 hash chain; optional HMAC anchor (F4). Unkeyed by default. No data is encrypted at rest and there is no TLS beyond what the provider SDK gives. |
| A03 | Injection | **Mitigated** | F1 fixed. No SQL, shell, `eval`, `pickle` or template engine in the code (a test scans for these sinks). HTML is built with `textContent`. The one CSV cell that can carry model-side text (the scorecard's reviewer note) always starts with `AUTO:`, so it cannot begin with a spreadsheet formula character. |
| A04 | Insecure design | **Mitigated** | Deterministic engine decides; AI only explains; fail-closed on unknown data, contract failure and rule crashes; write-ahead audit of every AI question. |
| A05 | Security misconfiguration | **Mitigated** | `.env` is ignored and not tracked (tested); `.env.example` holds no value; no debug mode; the only network endpoints are the two named model providers (tested). |
| A06 | Vulnerable and outdated components | **Mitigated** | `pip-audit` clean today. Exact pins mean this must be re-run when versions change. |
| A07 | Identification and authentication failures | **Closed in the reviewer API** (SPECS 10d); gap remains in the offline file-based flow | The API requires badge, password and an authenticator code, locks accounts after five failures, refuses replayed codes, answers all failures identically, and takes the reviewer from the session. The offline review page and `run_audited_review.py` still record self-declared text, by design. Residual risks are listed in SPECS 10d. |
| A08 | Software and data integrity failures | **Partial** | Release checksums for the pack; audit chain; engine code hash; no unsafe deserialization. Nothing signs the repository or the audit log itself beyond F4. |
| A09 | Logging and monitoring failures | **Partial** | Every check, AI action and decision is logged with versions and hashes; F3 fixed. There is no alerting or monitoring, and the log can be deleted by whoever can delete the files (the anchor copy is meant to be kept elsewhere). |
| A10 | Server-side request forgery | **Not applicable** | The service makes no request to an address supplied by data. The model endpoints are constants in the code. |

## Red-team run (2026-09-26)

More than 30 attacks through the real audited pipeline: hidden and blanked fields, unknown policy, forged tags in claim values, prompt injection in notes and documents, NaN and Infinity, a 5 MB field and 3,000 lines, unicode tricks, replays, a lying and a failing model, seven kinds of bad reviewer decision, and tampering with the log. No claim that violates one of the 15 rules got through unflagged. Log tampering was detected except for rewriting the chain and the anchor together without a key (keyed anchor detects it) and deleting both files. Replays and uncovered defects were the two new findings (F5, F6). Still open: a reviewer decision is accepted under any `actor` name, because there is no authentication.

## Residual risks, in priority order

1. **Authentication and authorisation** (A01, A07): built for the reviewer API (SPECS 10d) and tested with 21 attacks and 6 races on two stores. The offline file-based flow remains unauthenticated, and the residual risks in SPECS 10d (single-process address throttle, sessions of other devices surviving a password change, no password-reset flow, TLS left to the deployment) are open.
2. **PHI to an external model** (LLM02). Do not use real patient data with the hosted provider.
3. **Audit log is tamper-evident, not immutable** (A02, A08). Set `AUDIT_ANCHOR_KEY`, keep the anchor and key outside the writer's storage, and use write-once storage for real immutability (`docs/16`).
4. **No per-run AI spending limit** (LLM10).
5. **Model explanations can be misleading** even when they pass validation (LLM01, LLM09). Human review is the control.

## Reproduce

```bash
python -m unittest tests.test_security_owasp tests.test_stress_differential.RuleFactInjection
uv pip install bandit pip-audit detect-secrets
bandit -r src scripts -q ; pip-audit -r requirements.txt
```
