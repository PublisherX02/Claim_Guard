# 28 | Upgrades for Phase 2 (proposed, not built)

Nothing here is implemented. Each item names the measurement that motivates it, so it is an answer to a number, not a fashion.
Measurements are from `docs/27_Decisions_Proofs_and_Defense.md` and `outputs/defense/`.

| # | Upgrade | Motivated by | Status |
|---|---|---|---|
| 1 | **Return the verdict first, attach the AI explanation when it arrives.** The rules answer in about a millisecond; the explanation is the slow part. | AI step median 2.86 s, P95 28.9 s (117 live calls); the engine is about 1,750 times faster per claim | Phase 2 |
| 2 | **Hard time limit and circuit breaker on the AI call**, falling back to the template (which already exists). | The current timeout is 90 s, three times the observed P95 | Phase 2 |
| 3 | **Cache explanations by finding hash** (rule id, status and evidence shape). | Many findings repeat; every cache hit removes a model call and its cost | Phase 2 |
| 4 | **Write one audit append per claim and keep an anchor after every append.** | 55% of an append's time is the anchor rewrite; writing the anchor every 10 appends would be faster still but weakens truncation detection, so batching claims is the safe route | Phase 2 |
| 5 | **Constrained decoding when the model is served locally** (vLLM or SGLang with XGrammar, or Ollama structured outputs): the schema is compiled into a grammar so a malformed reply cannot be produced. | Schema rejections were 72 of 114 guard rejections in the ablation. The guards stay, because a structurally valid reply can still be wrong: an arXiv paper titled "Constrained Decoding Eliminates Structural Failures in Small LLMs but Reveals a Scale-Dependent Semantic Gap" makes that point, and we have not read past its title | Phase 2, with local serving |
| 6 | **Merkle-tree audit log with inclusion proofs and an externally held or time-stamped root** (Certificate-Transparency style, RFC 6962; RFC 3161 time-stamping). | The audit log is tamper-evident, not immutable. Without the HMAC key anyone who can write both files can rewrite the log and its anchor (tamper matrix, rows 6 and 8) | Phase 2 |
| 7 | **Keep the HMAC key off the writer's machine** (a secrets manager) and **alert when the strict verifier finds unanchored rows.** | The strict check now detects appended rows, but only if the anchor is trustworthy | Phase 2 |
| 8 | **Authentication and roles** before any web or mobile review interface. | Reviewer identity was self-declared | **Built** for the reviewer API (SPECS 10d); the offline flow is unchanged |
| 9 | **Re-run the Architecture B comparison on a stable, locally served tool-capable model.** | 10 of Architecture B's 12 runs hit hosted-endpoint failures, so the result is a floor, not its best case | Phase 2 |
| 10 | **Load test the whole path** (queue, workers, model server) once items 1 to 4 exist. | The engine was measured alone at about 500 to 1,300 claims/s per core; the full path has not been | Phase 2 |

## Dependencies: checked tonight

- `pip-audit -r requirements.txt`: no known vulnerabilities (run from a temporary environment).
- `yara-x==1.20.0` is what we pin, and it is the newest 2026 release the search found (1.20.0, 30 August 2026). One search result shows another project bumping to 1.20 to address a published advisory (GHSA-2jx3-ff3v-j7jj); we have not checked whether that advisory affects our use.
- We did not check newer `pydantic` or `openai` releases. Pins stay as they are until that is done.

## Sources from the search

- [YARA-X 1.20.0 Release, SANS Internet Storm Center](https://isc.sans.edu/diary/rss/33288)
- [yara-x on PyPI](https://pypi.org/project/yara-x/)
- [Constrained Decoding Eliminates Structural Failures in Small LLMs but Reveals a Scale-Dependent Semantic Gap (arXiv 2609.23742)](https://arxiv.org/pdf/2609.23742)
- [Structured outputs in vLLM, Red Hat Developer](https://developers.redhat.com/articles/2025/06/03/structured-outputs-vllm-guiding-ai-responses)
- [Lightweight Tamper-Evident Log Integrity Verification, Merkle-tree pipeline (arXiv 2605.00065)](https://arxiv.org/html/2605.00065v1)
- [AuditTrail-Ledger, RFC 6962 Merkle-tree audit logging (GitHub)](https://github.com/alinurettin/AuditTrail-Ledger)
