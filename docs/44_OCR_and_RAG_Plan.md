# 44 | Plan: an OCR system and a RAG system (what for, in what order, how to know it works)

> **Status (2026-10-13): built.** This page is the plan written before the build. What was built, how it was tested and what it does not do is in [docs/45](45_Document_Intake_Ledger_and_Streaming.md) section 1 (file import with the reader container, Tesseract and the Unlimited-OCR vision model) and section 5 (the rulebook question search, which is plain BM25 retrieval, not a language model and not hybrid). Section 8 below is the review of the colleague's repository and why it was not adopted. The statements in section 7 still hold: the measured figures are in docs/45 and are for synthetic documents only.

Status: **plan only, nothing built.** Written 2026-10-12 before the mentor meeting of 2026-10-13. The team was asked for both; nobody has said what they are for. This page proposes uses that fit what the system already does, ranks them, and lists the questions that decide between them. Every model name below is a candidate to be measured, not a result.

## 1. Where they would plug in (read from the code and the data, not assumed)

- A claim carries an `attachments` array: for each document `attachment_id, type, patient_id, service_code, service_date, document_status, text` (`schemas/claim.schema.json`). In the 600 development claims there are 372 documents: 186 final imaging reports, 171 final service notes, 15 draft imaging reports. Their `text` is a one-line synthetic sentence.
- Rule R010 "Required supporting document" decides on the **metadata** (type, patient, service, date, final/draft) and says: *"Content text is untrusted and cannot change these rules."* Today the metadata arrives already typed by whoever built the claim. In real life that metadata has to come from somewhere: a person fills it in, or the document is read. **That gap is the natural job for OCR.**
- The reviewer console shows evidence and an AI draft that is grounded in the rule's own text (the model never sees claim values). The rulebook is 15 rules plus an advisory pack. **That is the natural corpus for a first RAG.**
- The brief's bonus list names it: "multi-format attachment parsing (OCR/RAG PDF)", 2 points. The mentor's point 8 asks for local models only; the target server is one RTX 3080 (10 GB) with 12 GB of RAM, already serving gemma3:4b.

## 2. OCR: three possible jobs, in order of value

**O1. Attachment inventory builder (recommended first).** A scanned or photographed document (PDF, JPG, PNG) goes in; out come the schema fields (`type, patient_id, service_code, service_date, document_status`) plus the text, each with a confidence and the **page image kept next to it**. Nothing is trusted silently: a person confirms the extracted fields in the console (one click when they look right) before the claim enters the queue, and the confirmation is audited. The rule engine then runs on confirmed metadata exactly as today, so a misread cannot create or hide a finding by itself. This is the same trust boundary as the AI draft: the model proposes, the engine and a human decide.

**O2. Cross-check findings (advisory, after O1).** Compare what the document says with what the claim says (patient id, date, service, amount where a total appears). A mismatch becomes an advisory finding in the extension pack (E-series, separate from the official 15 as the existing extension rules are), never a change to R010.

**O3. Paper-claim intake ("Scan" screen).** A claims officer photographs a paper claim; OCR fills a draft claim for the officer to correct. Largest scope (needs line items, amounts), most error-prone; only if the mentor wants paper claims.

**Hard prerequisite: the upload gate.** No byte of an uploaded file reaches an OCR engine before a fail-closed check: magic bytes against the declared type, allow-list (PDF, JPEG, PNG), size and page caps, PDF structure scan that quarantines `/JS /JavaScript /OpenAction /Launch /AA /EmbeddedFile /XFA`, decompression-ratio cap for images, then rendering in an isolated worker with no network and a time limit. Written in Rust like the rest, with a corpus of hostile files as tests (the design is already in the project memory from the Attijari pipeline; offline only). Nothing here is built, and this is the part to build first because it is useful even if OCR itself changes.

**Engine candidates (to be measured, none verified here):**
| Candidate | Why | Doubt |
|---|---|---|
| Tesseract (CPU, Apache-2.0, Arabic and English models) | no GPU, deterministic, easy to run beside the API | weak on tables and low-quality scans |
| `baidu/Unlimited-OCR` (3B vision-language, MIT) | recorded in project notes as the intended model | whether llama.cpp's vision path supports its architecture was never checked; `trust_remote_code` if loaded through transformers |
| gemma3:4b through Ollama (already on the server, accepts images) | zero new runtime or download | probably the weakest reader of dense text; a baseline, not a favourite |

**How to know it works (measurable, no new data needed):** render the 372 synthetic documents (and, from the claims, invoices) into PDFs and photos with controlled damage (rotation, blur, JPEG noise, low resolution, stamps, an Arabic variant), run each engine, and score **field-level accuracy against the known ground truth** and **the share of documents a person would have to correct**. Report per damage level, with the failures shown. The deciding number is not character accuracy but: *how often does a confirmed-metadata claim end with a different rule result than the true metadata would give*; the console confirmation step is judged by whether it catches those. Do not claim a figure that was measured only on clean synthetic renders as a figure for real scans.

## 3. RAG: three possible jobs

**G1. Ask the rulebook (recommended first).** Corpus: the rules (id, title, logic, corrective action), the extension rules, the policy and catalogue files, and the project's own docs on how to read an evidence table. A reviewer asks "what does R006 say about EDU-SEPARATE?" or "why is this a high-severity rule?"; the answer is **extractive first**: the retrieved passages, quoted and cited by rule id and version, with at most a short generated connective sentence that the existing grounding guard checks (every quoted number and id must appear in a retrieved passage). With 15 rules a plain embedding index is enough; this is small on purpose.

**G2. Payer policy documents.** If a payer's real policy PDFs or contracts exist, G1's machinery over them answers "does this payer cover X" with citations. Needs O1's ingestion for the PDFs and a corpus we do not have. Ask the mentor whether such documents exist; do not invent them.

**G3. Precedents from past decisions.** "Claims like this were dismissed with this reason by seniors." Useful for consistency, but it feeds on reviewers' free-text reasons (personal data risk, anchoring risk, and it overlaps with the active-learning item 4 that waits for the mentor). Not before real decisions exist.

**Design decisions that keep it on-premises and small:**
- Embeddings from a local model served the way gemma3 is (Ollama; a multilingual model such as bge-m3 is a candidate because Arabic is likely). The vectors live in MongoDB (the system's store) and the search is a brute-force cosine in Rust: for a corpus of hundreds of passages that is milliseconds and needs no vector database. Revisit only if the corpus passes a few hundred thousand passages. (Whether MongoDB Community's own vector search is available in the version we run was not checked.)
- Every answer shows its sources; an answer with no retrieved passage above a score threshold says "not in the documents" instead of generating.
- The model never sees claim values (same rule as today's explanation step); a RAG question is about the rulebook, not about a patient.
- Prompt injection: documents that are ingested are untrusted data. Retrieved text is fenced and labelled, and the answer is checked against it; the security suite gets the same multilingual and payload-splitting tests the explanation step has.

**How to know it works:** write 40 to 60 questions from the rulebook with the expected rule ids (hand-written by us, plus the questions the mentor asks), score **retrieval hit rate at k=1/3** and **faithfulness by mechanical check** (every cited id and number exists in a retrieved passage), and count refusals on questions that have no answer. The Ragas faithfulness metric uses an LLM as judge, which is circular here, so it is at most a side figure.

## 4. Memory and time budget on the target (one RTX 3080 10 GB, 12 GB RAM)

gemma3:4b is about 3.3 GB resident; a 3B vision-language model is 2 to 6 GB depending on the quantisation; an embedding model is under 1 GB. Run them one at a time (load on demand, unload after a timeout) and keep the model server's `--max-num-seqs` equal to the worker concurrency as now. OCR is a batch step at intake, not on the reviewer's clicking path, so seconds per page are acceptable; the AI explanation step keeps its 90-second ceiling and its template fallback. **None of these figures were measured on the target.**

## 5. Order of work if the mentor says "yes, both"

1. Upload gate (Rust) with hostile-file tests. Useful whatever happens.
2. The OCR evaluation harness: synthetic damaged documents with ground truth, and a scoring script. Choose the engine from numbers.
3. O1 end to end: upload, gate, OCR, extracted fields beside the page image in the console, confirm step, audit entry, queue. Desktop and console only.
4. G1: ingest the rulebook, embeddings, retrieval with citations, the question box in the console, the 40-60 question evaluation.
5. O2 advisory cross-checks, then G2 or G3 if the mentor supplies a reason or a corpus.

## 6. Questions for the mentor (these decide the plan)

1. What are the OCR and RAG for in his picture: reading scanned supporting documents, reading paper claims, answering reviewers' questions about policy, or something else? Which one would he check in a demo?
2. Do real attachments exist in the target hospital's workflow (scans, PDFs, photos), in which languages (Arabic, English), and do paper claims have to be supported?
3. Is there a corpus for RAG besides our 15 rules (payer contracts, coding guides, hospital policies), and may we use it?
4. Must the confirmation step stay (a person confirms extracted fields), or does he expect fully automatic extraction? (The plan keeps the human step; say so.)
5. Is there an accuracy bar he expects (field-level accuracy, share needing correction)?

## 7. What not to claim

- That OCR or RAG exists, that any model "reads" documents well, or any accuracy figure, before the harness has produced it on damaged documents.
- That synthetic renders stand for real scans.
- That RAG decides anything: it quotes and cites; the engine decides status and a person decides the claim.

## 8. Review of the colleague's repository (github.com/ayechiahmed/clinicProj, one commit "Initial commit", 17 files) - VERDICT: do not keep as a component; keep three ideas and the sample files
What it is: `extractor.py` (370 lines: file type dispatch, PDF text layer with per-page Tesseract fallback, DOCX, XLSX, CSV, images, then an LLM prompt that turns the raw text into JSON), `document_loader.py` (500-character sliding-window chunks), `agent.py` (863 lines: a LangGraph ReAct agent on Google Gemini, FAISS index of the policy chunks with all-MiniLM-L6-v2, a calculator tool, a very long system prompt). `server.py` and `sys prompt.txt` are empty files. The only policy corpus is 7 lines (`policies/sample_policy.txt`). No tests, no requirements file.
Why it does not fit:
1. **The LLM decides the findings.** The agent is told to validate the claim against retrieved rules and return findings, severity and confidence. That reverses the rule of the brief and of this project: the rule engine decides status, the model only helps read it. A model-decided finding has no oracle, cannot be reproduced, and cannot be scored (our engine: F1 1.0 on the answer key, 0 false positives on valid claims).
2. **Claim data goes to Google's cloud** (`ChatGoogleGenerativeAI`, `GOOGLE_API_KEY`). The mentor's points 6 and 8 (Middle East data protection, everything on the hospital's servers) rule this out. Our stack is on-premises only (vLLM).
3. **The extracted JSON is not our claim format.** Its schema (`patient{id,name}`, `claim_lines`, `amounts`...) does not pass `transport::validate`; nothing converts it, nothing validates it, nothing quarantines a bad extraction. `claim.csv` is not a CSV: it is one JSON claim from our data/stress pack with a misleading name.
4. **No upload gate.** Any file type by extension, no magic-byte check, no size or page cap, no PDF structure scan (JS, launch actions, embedded files), no timeout. `pdfplumber` and `pypdfium2` would parse hostile files in the main process.
5. **The calculator tool uses `eval`** behind a character allow-list that permits `**`, so `9**9**9**9` is a denial of service (the same hole fixed in our Python tree as "calculator `**` guard").
6. The guard against unsafe input is three regular expressions about violence; it does not address prompt injection or invented clinical content. Hard-coded Windows Tesseract path; English-only embedding model; flat-L2 FAISS over 7 lines of text.
What is worth keeping (and is now in the build, re-implemented): (a) the dispatch idea "text layer first, OCR only when a page has no text"; (b) the principle in `agent.py` that the claim is never embedded, only the policy documents are; (c) "use null for anything not in the text, never guess" with a retry when the model returns invalid JSON (our version validates against the claim schema and quarantines instead of retrying forever). The four sample images (claim1.png, claim2.png, aaa.jpg, sample_invoice.png) and sample.pdf are useful as hostile-realistic test inputs: they are not in our field layout (different ids, CPT-style codes, an "Expected outcome" box that talks to the system), so they test that conversion does not guess and that text inside a document is data, not instructions. Copied only as test fixtures after review; no code was run from that repo.
