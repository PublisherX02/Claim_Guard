# 52 | Asking the rulebook in the operator's own words: BM25 vs meaning-based search vs a Laya gate

**Status: the dense multilingual retriever is wired into the product (2026-10-18); Laya and the hybrid are not.** "Ask the rulebook" ranks by meaning when an embedding service is configured and reachable, and by BM25 otherwise (section 7). Code and data of the experiment: `experiments/rag_semantic/` (`python experiments/rag_semantic/run.py`; the run needs a newer `transformers` than the project's, see the header of `run.py`). Results: `results.json` (final), `results_first_run.json` (the first run, kept on record). The product reproduces the numbers below: `python rust/tools/rag_parity.py` (section 7).

## 1. The problem and the idea

BM25 matches words. An operator who has not memorised the manual says "the same procedure appears twice", not "possible duplicate service lines", and gets refused. The idea (from the "Jev + RAG" description): keep retrieval, then have a calibrated judge decide which retrieved passages really help answer the question, and refuse when none does. Laya takes the place of Jev: it is a yes/no judge with a probability (`noul` question type), supports 100+ languages and runs in about 60 ms for ten candidates. The answer stays what it is today: the quoted passage and its source. Nothing is generated.

## 2. How it was measured

- **Same corpus as the product**: the 111 passages `rag.rs` builds from the 15 rules and the payer table. The Python port of the Rust retriever reproduces the Rust test exactly (hit@1 88%, hit@3 96%, the same single miss, 5/5 off-topic refused) before anything else is compared.
- **A new question set, frozen before any dense or Laya retriever was run** (`questions_paraphrase.json`): 42 English questions in operator language, 18 of them sharing no word at all with their target passages and 36 sharing at most one; 12 French and Arabic questions; 17 off-topic questions, 8 of them "near the domain" but not answered by the documents ("how do I appeal a rejected claim"). A hit means the right rule or policy is among the top three. Half the questions are `dev`, half `test`; **every threshold was chosen on `dev` only** and the tables quote `test`.
- Systems: BM25 (the product); dense search (cosine of sentence embeddings, `all-MiniLM-L6-v2` English-only, and `paraphrase-multilingual-MiniLM-L12-v2`); a hybrid of BM25 and dense (reciprocal-rank fusion); Laya judging the dense top 5 or top 10, either re-ranking by its probability or only gating.
- Two variants were added after the first run, and are labelled post-hoc: `hybrid2` (the BM25 side only contributes hits the Rust retriever itself would accept) and Laya judging the **dense** candidates instead of the hybrid's (the first run fed Laya a poor candidate list, which caps what it can do).

## 3. Results (test half; a refusal counts as a miss)

| | BM25 (product) | Dense, English model | Dense, multilingual | Hybrid (BM25+dense) | Laya gate on dense top 10 |
|---|---|---|---|---|---|
| English paraphrases, hit@3 (n=21) | **5%** | 76% | **81%** | 62% | **81%** |
| ...of which refused though answerable | **76%** | 14% | 5% | 5% | 0% |
| English, 0 shared words, hit@3 (n=8) | 0% | 50% | 75% | 38% | 75% |
| French + Arabic, hit@3 (n=6) | 0% | 0% | **100%** | 83% | 83% |
| Original 26 questions, hit@3 | 96% | 96% | 96% | 96% | 96% |
| Off-topic, pure (n=4) refused | 100% | 100% | 100% | 100% | 75% |
| Off-topic, near the domain (n=4) refused | 75% | 50% | **25%** | 25% | 50% |

Laya's relevance probabilities, as a calibration check on 350 test candidates: ECE 0.12 (zero-shot, not calibrated for this task). Latency of the Laya step: p50 63 ms, p95 71 ms for ten candidates.

## 4. What this says

1. **The problem is real and large.** On operator-style wording the product's BM25 answers almost nothing (hit@3 5%, 76% refused). Any embedding retriever fixes most of it (76 to 81%) with hit@3 on the original 26 questions unchanged (96%). Top-1 on those 26 is 96% with the English model but 81% with the multilingual one (BM25: 88%), so the multilingual model finds the right passage but sometimes ranks it second.
2. **Dense search alone does the work.** The BM25 hybrid is no better than dense alone (it was worse in the first run and no better after the gentler fix), so it is not worth the extra moving part.
3. **The multilingual model is the one that matters for French and Arabic** (100% on six questions where BM25 and the English model get 0). That is six questions drafted by the assistant and not reviewed by a native speaker: a sign, not a measurement.
4. **Laya as a zero-shot gate does not beat a plain similarity threshold.** Judging the dense top 10 it ties the dense retriever on answerable questions (81%) and loses on pure off-topic refusal (75% against 100%); re-ranking by its probability does not help. It adds a 421M-parameter model and about 60 ms for no measured gain. This matches docs/50: the same model, used as a judge of something it was not trained for, adds nothing. A fine-tune on question/passage pairs could change that; it has not been tried.
5. **The honest weakness is the same for every dense variant: questions near the domain.** "How do I appeal a rejected claim" gets the nearest rule passage back, because a similarity threshold cannot tell "close in topic" from "answers the question" (multilingual model refuses 1 of 4). Nothing is invented (the answer is still a quotation), but it is misleading. If dense search is adopted, the console should show the match as "closest passage, may not answer your question", not as an answer.

## 5. What is not measured

- **Small numbers.** 21, 8, 6 and 4 test questions in the slices above; one question is 5 to 25 points. Differences of a few points are noise and the intervals are wide. Dev/test is one split, not repeated.
- **Written by the assistant, not by operators**, even though it was frozen before the retrievers ran. Real operator questions would be the real test. The French and Arabic are unreviewed.
- **Thresholds are tuned on 27 answerable and 9 off-topic dev questions.** Another split would pick other thresholds.
- **Laya was not fine-tuned for relevance.** Only the zero-shot checkpoint was tried; a fine-tuned judge, with training pairs kept out of the test questions, is the open step.
- **Nothing is wired.** Serving a sentence-embedding model from the Rust server needs either an ONNX model loaded in Rust or a small model service; BM25 would stay as the fallback when it is unavailable. That is a separate decision.
- **Latency and memory were measured on the development laptop's GPU**, not on the target server.

## 7. What was wired into the product, and how it was checked

**Shipped**: exactly the `dense_minilm_multi` system of section 3 and nothing else: `paraphrase-multilingual-MiniLM-L12-v2` (revision `e8f8c211226b894fcb81acc59f3b34ba3efd5f42`), cosine of normalised embeddings, refusal when the closest passage is below 0.29 (chosen on the dev half), the top four passages one per (source, kind). Laya and the BM25 hybrid are left out because section 4 shows they add nothing. No "BM25 first, dense if unsure" variant was added, because it was not measured and the numbers here would stop describing the product.

**How it runs**: a sidecar (`deploy/embed/service.py`, own container, CPU, weights baked in at the pinned revision, `HF_HUB_OFFLINE=1`, bearer token, network with no route out, compose profile `search`) turns texts into vectors. The Rust server (`cg-engine/src/rag.rs`, `payer.rs`, `cg-cli/src/embedder.rs`) embeds the 111 passages on first use and caches the vectors by hash of the passage text; an approved policy change re-embeds only the policy passages (their label names the table version), not the rules. It embeds the question per request, ranks by cosine and answers with the quoted passage as before. Switched on by `EMBEDDER_URL`, `EMBEDDER_TOKEN` (and `EMBEDDER_THRESHOLD`, default 0.29: it belongs to this model).

**What the user sees**: every semantic answer carries the caveat "closest passage by meaning; it may not answer your question" (section 4.5), and the console shows it. If the service is not configured the keyword search answers exactly as before (`mode: keyword`). If it is configured but down, slow or answering garbage, the server answers with the keyword search, says so (`fallback` in the reply, shown in the console), and leaves the service alone for 20 seconds so a dead sidecar costs one failed call, not one per question.

**Check that production equals the experiment** (`rust/tools/rag_parity.py`, 2026-10-18, output `outputs/defense/rag_parity.json`): the CPU sidecar and the Rust cosine, through the real HTTP route, on the frozen question sets, against `results.json` (which was a GPU run in Python). All slices agree to four decimals:

| Slice | hit@1 | hit@3 | false refusal | off-topic refused |
|---|---|---|---|---|
| English paraphrases, test (n=21 + 8 off-topic) | 0.4286 | 0.8095 | 0.0476 | 0.625 |
| French + Arabic, test (n=6) | 0.6667 | 1.0000 | 0 | |
| Original 26 + 5 off-topic | 0.8077 | 0.9615 | 0 | 1.0 |
| Off-topic, pure (n=4) | | | | 1.0 |
| Off-topic, near the domain (n=4) | | | | 0.25 |

With the sidecar stopped, the keyword fallback reproduces BM25's numbers on the original 26 (hit@1 0.88, hit@3 0.96).

**What got worse, said plainly**: on the original 26 questions top-1 falls from 88% (BM25) to 81% (hit@3 is unchanged at 96%), and near-domain questions are refused 25% of the time instead of 75%: "how do I appeal a rejected claim" now returns the nearest rule. That is why the caveat is on every semantic answer. The numbers are on small sets written by the assistant (section 5); real operator questions are still the open test, and nothing here was run on the target server.
