# 60. "Ask the rulebook": a knowledge graph (KAG) and a cache (CAG) in front of passage search

## What was built

"Ask the rulebook" used to do one thing: find the four passages closest to the question (BM25 or bge-m3). That works for "what does R008 check?" and fails for
questions about how the parts connect: *which rules can fire for SVC-IMAGE under EDU-PLUS?* needs the policy table, the rule texts and the field dictionary
together. Two tiers now sit in front of passage search (`PayerRules::ask_routed`, `GET /api/knowledge?q=..&generate=1`, `cg ask`):

| Tier | What it is | Where |
|---|---|---|
| CAG (cache) | answers kept in memory, keyed by the normalized question, the options, the policy-table version and a hash of the rulebook; the rulebook (about 3.6k tokens) can also be the front of every model prompt so a server that caches prompt prefixes computes it once | `cg-engine/src/cag.rs` |
| KAG (graph) | a graph built from `rules.json`, the policies, services, providers, diagnoses and the field dictionary (`used_by`); a closed set of 11 queries; each answer is a list of cited facts computed from the files, no model needed | `cg-engine/src/kag.rs` |
| Passage search | unchanged, used when the graph does not understand the question | `rag.rs` |

A model (Gemma 3 4B through Ollama, `gemma3-ask:4b` with an 8192-token context) is optional and has two jobs, both fenced:

* **Choosing a query** when the built-in keyword/identifier planner cannot (French, Arabic, loose wording). The proposal must be one of the 11 operations,
  every identifier must exist in the graph, extra keys are refused, and an id the model puts in the query must also be written in the question.
* **Writing the facts up** as two to five sentences. The text is shown only if every sentence cites a fact `[n]` and every rule id, service code and number
  in it occurs in the facts; otherwise it is dropped and the reason is returned. The cited facts are always returned beside it.

A cached answer is unreachable after a policy table is approved (the table version is in the key), so an old answer cannot outlive the rule it quoted.

## Measured (RTX 4060 Laptop 8 GB, 60 questions, `experiments/kag_cag/`, results in `outputs/defense/kag_cag_eval.json`)

A question counts as answered when every gold token (rule id, value, policy id) is in the returned text.

| Phase | Answered | Median latency |
|---|---|---|
| A. passage search only (before this work) | 34/60 | 23 ms |
| B. graph + passage search | 57/60 (52 by the graph) | graph about 0 ms; passage 31 ms |
| B round 2 (answer cache) | 57/60 | about 0 ms |
| C. + Gemma chooses the query | 57/60 (53 by the graph) | 3.0 s when the model is asked |
| D. + Gemma writes the answer | 57/60; 44 write-ups shown, 15 refused by the check | 3.7 s (graph), 7.0 s (passages) |
| E. D with the rulebook as the prompt front | 57/60; 43 shown, 16 refused | 3.5 s, 7.1 s |

Reading it honestly:

* The graph is what moved the number (34 to 57), and it is instant. The cache turns repeats into a lookup.
* The model as planner added one question (a field-name question) and nothing on accuracy. Its first version **lowered** accuracy to 52: on French and
  Arabic questions it copied an example and the graph answered "what rule R001 checks" with confidence. That is why an id must now appear in the question.
* Written answers are slower than the facts they restate, and 15 of 59 were refused by the check, so the facts stay the product; the write-up is a
  convenience. The prefix did not change the outcome (43 against 44 shown).
* The prefix probe on Ollama: the second call of the same 3,588-token prefix took 275 ms to process against 518 ms the first time, so some caching occurs, but
  the server still reports the whole prompt as processed. The large saving is expected from a server with real prefix caching (vLLM); not measured here.

Misses (3 of 60): the "does EDU-BASIC require an authorization for SVC-LAB" yes/no wording, the "which rules depend on R007" transitive wording, and one Arabic question.

## Regression on held-out sets (`outputs/defense/kag_cag_regression.json`)

The earlier retrieval experiment's held-out sets (20 answerable, 24 off-topic, English/French/Arabic): the graph took **0** answerable and **0** off-topic
questions, and the top three passages did not change for any answerable question. The first run took one off-topic question ("which documents does a
patient need to bring to the clinic") because it shares a word with the rulebook; the planner now needs a policy, insurer or claim word beside a bare
attribute word, and a test pins it.

## Limits

* The 60 questions were written by the author of the built-in planner; the 52 that the graph answers are therefore **not** an out-of-sample figure. The
  French/Arabic questions and the regression sets are the closest to out-of-sample. The rule-to-attribute gold is hand-derived from the rule texts (evidence tier C).
* The model is Gemma 3 4B, not Gemma 4. Latency is for this laptop, not the target server.
* Graph answers are only as good as the files they are built from; a rule whose text does not name the attribute it reads is missing from the graph.
* The cache lives in one process; it is not shared across replicas.

## Reproduce

```
ollama create gemma3-ask:4b -f experiments/kag_cag/Modelfile
python experiments/kag_cag/build_questions.py
python experiments/kag_cag/run_eval.py          # about 15 min; starts the embedding sidecar on the GPU
python experiments/kag_cag/run_regression.py
cg ask "Which rules can fire for SVC-IMAGE under EDU-BASIC?"   # --generate for the written answer
```
