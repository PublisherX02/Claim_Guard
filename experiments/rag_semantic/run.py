"""Does meaning-based retrieval, and a Laya relevance gate, beat BM25 for "Ask the rulebook"?  (experiment, not wired into the product)

    PYTHONPATH="<newer transformers>;<laya package>" python experiments/rag_semantic/run.py [--no-laya]

Systems, all over the same 111 passages as rag.rs:
  bm25        the Rust retriever, ported (reproduces the Rust test: hit@1 88%, hit@3 96%, 5/5 off-topic refused)
  dense_*     cosine of sentence embeddings; refuses when the best cosine is below a threshold
  hybrid_*    reciprocal-rank fusion of BM25 and dense; refuses on the same dense threshold
  laya_*      the hybrid's top five candidates judged by Laya ("does this passage help answer the query?", a yes/no question with a
              probability); refuses when no candidate reaches the threshold. Zero-shot: the checkpoint is NOT fine-tuned for this.
Every threshold is chosen on the paraphrase set's dev half only; the tables quote the test half and the original 26 questions.
Nothing is generated: the answer stays the quoted passage and its source.
"""
import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from bm25_port import BM25, load_corpus, tokens  # noqa: E402

ROOT = HERE.parents[1]
LAYA_DIR = ROOT / "experiments" / "laya_kaggle" / "laya_base"
MODELS = {"minilm_en": "sentence-transformers/all-MiniLM-L6-v2",
          "minilm_multi": "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"}
TOPN = 5
RRF_K = 60


def dedupe(order, ps, scores, n=TOPN):
    out, seen = [], set()
    for i in order:
        key = (ps[i]["source"], ps[i]["kind"])
        if key in seen:
            continue
        seen.add(key)
        out.append({"i": int(i), "source": ps[i]["source"], "score": float(scores[i])})
        if len(out) >= n:
            break
    return out


def bm25_ranked(ix, q):
    hits = ix.search(q, TOPN)
    idx = {(p["source"], p["kind"], p["text"]): i for i, p in enumerate(ix.ps)}
    ranked = [{"i": idx[(h["source"], h["kind"], h["text"])], "source": h["source"], "score": h["score"]} for h in hits]
    return ranked, (hits[0]["score"] if hits else -1.0)


def bm25_order(ix, q):
    raw = ix.raw_scores(q)
    s = np.array([r[0] for r in raw])
    order = [int(i) for i in np.argsort(-s, kind="stable") if s[i] > 0]
    return order


def bm25_accepted_order(ix, q):
    """Only the passages the Rust retriever itself would return (score >= 1.2 and >= 2 matched terms), best first."""
    idx = {(p["source"], p["kind"], p["text"]): i for i, p in enumerate(ix.ps)}
    return [idx[(h["source"], h["kind"], h["text"])] for h in ix.search(q, 1000)]


def evaluate(rows, ranked, conf, t, k_list=(1, 3)):
    """rows: question dicts (with 'want' or not). Returns hit@k (a refusal is a miss), false-refusal rate, and refusal rate for off-topic."""
    out = {}
    ans = [(r, ranked[r["id"]], conf[r["id"]]) for r in rows if "want" in r]
    off = [(r, ranked[r["id"]], conf[r["id"]]) for r in rows if "want" not in r]
    if ans:
        for k in k_list:
            out[f"hit@{k}"] = float(np.mean([c >= t and any(h["source"] in r["want"] for h in rk[:k]) for r, rk, c in ans]))
        out["false_refusal"] = float(np.mean([c < t for _, _, c in ans]))
        out["n_ans"] = len(ans)
    if off:
        out["off_refused"] = float(np.mean([c < t for _, _, c in off]))
        out["n_off"] = len(off)
    return out


def tune(dev_rows, ranked, conf, grid):
    """Smallest threshold maximising 0.5*hit@3 + 0.5*off-topic refusal on the dev half (ties go to refusing less)."""
    best = None
    for t in grid:
        m = evaluate(dev_rows, ranked, conf, t)
        j = 0.5 * m["hit@3"] + 0.5 * m["off_refused"]
        if best is None or j > best[0] + 1e-9:
            best = (j, float(t))
    return best[1], best[0]


def ece(p, y, bins=10):
    p, y = np.asarray(p, float), np.asarray(y, float)
    edges = np.linspace(0, 1, bins + 1)
    tot = 0.0
    for lo, hi in zip(edges[:-1], edges[1:]):
        m = (p > lo) & (p <= hi) if lo > 0 else (p >= lo) & (p <= hi)
        if m.any():
            tot += m.mean() * abs(y[m].mean() - p[m].mean())
    return float(tot)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-laya", action="store_true")
    a = ap.parse_args()

    ps = load_corpus()
    ix = BM25(ps)
    para = json.loads((HERE / "questions_paraphrase.json").read_text(encoding="utf-8"))
    rust = json.loads((HERE / "questions_rust26.json").read_text(encoding="utf-8"))

    rows = []
    for x in para["answerable"]:
        vocab = set()
        for p in ps:
            if p["source"] in x["want"]:
                vocab |= set(p["tokens"])
        overlap = len(set(tokens(x["q"])) & vocab) if x["lang"] == "en" else None
        rows.append({**x, "set": "para", "overlap": overlap})
    for x in para["off_topic"]:
        rows.append({"id": x["id"], "q": x["q"], "split": x["split"], "set": "para", "kind": x["kind"]})
    for i, x in enumerate(rust["answerable"]):
        rows.append({"id": f"R{i + 1:02d}", "q": x["q"], "want": x["want"], "split": "rust", "set": "rust", "lang": "en"})
    for i, q in enumerate(rust["off_topic"]):
        rows.append({"id": f"RO{i + 1:02d}", "q": q, "split": "rust", "set": "rust", "kind": "pure"})

    # ---- 1. BM25 (the Rust retriever) -----------------------------------------------------------------------
    systems = {}
    r_bm, c_bm = {}, {}
    for r in rows:
        r_bm[r["id"]], c_bm[r["id"]] = bm25_ranked(ix, r["q"])
    systems["bm25"] = (r_bm, c_bm, [0.0])             # native refusal: no hit, no answer

    # ---- 2. dense and hybrid ------------------------------------------------------------------------------
    from sentence_transformers import SentenceTransformer
    ptexts = [f"{p['source']} ({p['kind']}): {p['text']}" for p in ps]
    hybrid_cache = {}
    for name, mid in MODELS.items():
        t0 = time.time()
        model = SentenceTransformer(mid, device="cuda")
        P = model.encode(ptexts, normalize_embeddings=True, batch_size=64)
        Q = model.encode([r["q"] or " " for r in rows], normalize_embeddings=True, batch_size=64)
        S = Q @ P.T
        r_d, c_d, r_h, c_h, r_h2, c_h2, r_d10 = {}, {}, {}, {}, {}, {}, {}
        for qi, r in enumerate(rows):
            s = S[qi]
            order = list(np.argsort(-s, kind="stable"))
            r_d[r["id"]] = dedupe(order, ps, s)
            r_d10[r["id"]] = dedupe(order, ps, s, n=10)
            c_d[r["id"]] = float(s[order[0]]) if r["q"] else -1.0
            # reciprocal-rank fusion with BM25 (only passages BM25 scores above zero take part on that side)
            fused = np.zeros(len(ps))
            for rank, i in enumerate(order):
                fused[i] += 1.0 / (RRF_K + rank + 1)
            for rank, i in enumerate(bm25_order(ix, r["q"])):
                fused[i] += 1.0 / (RRF_K + rank + 1)
            forder = list(np.argsort(-fused, kind="stable"))
            r_h[r["id"]] = dedupe(forder, ps, fused)
            c_h[r["id"]] = c_d[r["id"]]
            fused2 = np.zeros(len(ps))                        # post-hoc variant: BM25 side = accepted hits only
            for rank, i in enumerate(order):
                fused2[i] += 1.0 / (RRF_K + rank + 1)
            for rank, i in enumerate(bm25_accepted_order(ix, r["q"])):
                fused2[i] += 1.0 / (RRF_K + rank + 1)
            r_h2[r["id"]] = dedupe(list(np.argsort(-fused2, kind="stable")), ps, fused2)
            c_h2[r["id"]] = c_d[r["id"]]
        grid = np.round(np.arange(0.0, 0.9, 0.01), 2)
        systems[f"dense_{name}"] = (r_d, c_d, grid)
        systems[f"hybrid_{name}"] = (r_h, c_h, grid)
        systems[f"hybrid2_{name}"] = (r_h2, c_h2, grid)
        hybrid_cache[name] = r_d10
        print(f"[{name}] embedded {len(ps)} passages + {len(rows)} questions in {time.time() - t0:.1f}s", flush=True)
        del model

    # ---- 3. Laya as the gate on the dense retriever's top candidates (multilingual model) -----------------------
    laya_info = {}
    if not a.no_laya:
        import laya
        agent = laya.Agent(str(LAYA_DIR), device="cuda")
        base = hybrid_cache["minilm_multi"]                                   # dense top 10, deduped by (source, kind)
        probs, lat = {}, []
        for r in rows:
            cands = base[r["id"]]
            qs = {f"c{j}": {"type": "noul", "instructions": "Does this passage help answer the query? Passage: " + ps[c["i"]]["text"]}
                  for j, c in enumerate(cands)}
            t0 = time.perf_counter()
            res = agent.predict(r["q"] or " ", qs)["answers"]
            lat.append((time.perf_counter() - t0) * 1000)
            probs[r["id"]] = [float(res[f"c{j}"]["noul"]) for j in range(len(cands))]
        grid = np.round(np.arange(0.0, 1.0, 0.01), 2)
        for n in (5, 10):
            r_lr, c_lr, r_lg, c_lg = {}, {}, {}, {}
            for r in rows:
                cands, p = base[r["id"]][:n], probs[r["id"]][:n]
                order = sorted(range(len(cands)), key=lambda j: (-p[j], j))
                r_lr[r["id"]] = [{**cands[j], "score": p[j]} for j in order]              # reranked by Laya's probability
                c_lr[r["id"]] = max(p) if p else -1.0
                r_lg[r["id"]] = [{**cands[j], "score": p[j]} for j in range(len(cands))]  # dense order kept, Laya only gates
                c_lg[r["id"]] = c_lr[r["id"]]
            systems[f"laya_top{n}_rerank"] = (r_lr, c_lr, grid)
            systems[f"laya_top{n}_gate_only"] = (r_lg, c_lg, grid)
        laya_info["latency_ms_per_question_p50_top10"] = round(float(np.percentile(lat, 50)), 1)
        laya_info["latency_ms_per_question_p95_top10"] = round(float(np.percentile(lat, 95)), 1)
        pp, yy = [], []                                                              # relevance calibration on the test half
        for r in rows:
            if r.get("split") != "test":
                continue
            for c, p in zip(base[r["id"]][:10], probs[r["id"]][:10]):
                pp.append(p)
                yy.append(1 if ("want" in r and c["source"] in r["want"]) else 0)
        laya_info["relevance_ece_test"] = round(ece(pp, yy), 4)
        laya_info["relevance_test_candidates"] = len(pp)

    # ---- 4. tune on dev, report on test ------------------------------------------------------------------
    dev = [r for r in rows if r["set"] == "para" and r["split"] == "dev"]
    slices = {
        "en test (all)": [r for r in rows if r["set"] == "para" and r["split"] == "test" and (r.get("lang") == "en" or "want" not in r)],
        "en test, <=1 shared word": [r for r in rows if r["set"] == "para" and r["split"] == "test" and r.get("lang") == "en" and "want" in r and r["overlap"] <= 1],
        "en test, 0 shared words": [r for r in rows if r["set"] == "para" and r["split"] == "test" and r.get("lang") == "en" and "want" in r and r["overlap"] == 0],
        "fr+ar test": [r for r in rows if r["set"] == "para" and r["split"] == "test" and r.get("lang") in ("fr", "ar")],
        "original 26 + 5 off-topic": [r for r in rows if r["set"] == "rust"],
    }
    report = {"corpus_passages": len(ps), "tuned_on": "paraphrase dev half", "laya": laya_info, "systems": {}}
    for name, (ranked, conf, grid) in systems.items():
        t, j = (grid[0], None) if len(grid) == 1 else tune(dev, ranked, conf, grid)
        report["systems"][name] = {"threshold": t, "dev_objective": j, "slices": {s: evaluate(rs, ranked, conf, t) for s, rs in slices.items()}}
        # off-topic split by kind on the test half
        off_test = [r for r in rows if r["set"] == "para" and r["split"] == "test" and "want" not in r]
        for kind in ("pure", "near-domain"):
            sub = [r for r in off_test if r["kind"] == kind]
            report["systems"][name]["slices"][f"off-topic test ({kind})"] = evaluate(sub, ranked, conf, t)
    (HERE / "results.json").write_text(json.dumps(report, indent=1), encoding="utf-8")

    def pct(x):
        return "-" if x is None else f"{100 * x:.0f}%"
    print("\n(tuned threshold in brackets; hit counts a refusal as a miss)\n")
    for s in list(slices) + ["off-topic test (pure)", "off-topic test (near-domain)"]:
        print(f"== {s}")
        n_a = report["systems"]["bm25"]["slices"][s].get("n_ans"); n_o = report["systems"]["bm25"]["slices"][s].get("n_off")
        print(f"   (n answerable = {n_a}, n off-topic = {n_o})")
        print(f"{'system':26} {'thr':>5} {'hit@1':>6} {'hit@3':>6} {'false-refusal':>14} {'off refused':>12}")
        for name, rep in report["systems"].items():
            m = rep["slices"][s]
            print(f"{name:26} {rep['threshold']:>5} {pct(m.get('hit@1')):>6} {pct(m.get('hit@3')):>6} {pct(m.get('false_refusal')):>14} {pct(m.get('off_refused')):>12}")
        print()
    print("laya:", laya_info)


if __name__ == "__main__":
    main()
