"""Two upgrades to "Ask the rulebook", measured on a test set frozen before they were written (questions_v3_test.json):

  phrases  Arabic and French ways of asking about each rule, appended to the rule's title passage for the EMBEDDING only (never quoted)
  decoys   out-of-scope topics (appeals, copayment, premiums, ...) as passages; a question whose closest passage is a decoy is refused

    HF_HOME=D:/hf_cache python experiments/rag_v2/run_v3.py [--models minilm_multi,bge_m3]

Thresholds are chosen on the dev rows of questions_v2.json and docs/52's paraphrase set; every number quoted is on questions_v3_test.json
(20 answerable, 24 off-topic). Writes results_v3.json and graphs in docs/img/rag_v2/.
"""
import argparse
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run as base  # noqa: E402

PHRASES = HERE.parents[1] / "rust" / "crates" / "cg-engine" / "data" / "rule_phrases.json"
TOPN = base.TOPN


def corpus(phrases, decoys):
    ps = base.load_corpus() + base.field_passages()
    spec = json.loads(PHRASES.read_text(encoding="utf-8"))
    if phrases:
        for p in ps:
            if p["kind"] == "title" and p["source"].startswith("rule R"):
                r = spec["rules"].get(p["source"].split()[1])
                if r:
                    p["aliases"] = "، ".join(r["ar"]) + " | " + " ; ".join(r["fr"])
    if decoys:
        for d in spec["decoys"]:
            ps.append({"source": "out of scope: " + d["topic"], "kind": "out of scope", "text": d["text"], "aliases": "، ".join(d["ar"]) + " | " + " ; ".join(d["fr"]), "tokens": []})
    return ps


def v3_rows():
    q = json.loads((HERE / "questions_v3_test.json").read_text(encoding="utf-8"))
    return [{**x, "set": "v3"} for x in q["answerable"]] + [{**x, "set": "v3"} for x in q["off_topic"]]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default="minilm_multi,bge_m3")
    a = ap.parse_args()
    import torch
    from sentence_transformers import SentenceTransformer

    dev = [r for r in base.load_rows() if r["split"] == "dev"]
    test = v3_rows()
    rows = dev + test
    labels = base.field_labels()
    grid = np.round(np.arange(0.0, 1.0, 0.005), 3)
    out = {}
    for name in a.models.split(","):
        mid, qp, pp = base.MODELS[name]
        model = SentenceTransformer(mid, device="cuda")
        model.max_seq_length = 256
        model.half()
        for phrases in (False, True):
            for decoys in (False, True):
                ps = corpus(phrases, decoys)
                texts = [pp + f"{p['source']} ({p['kind']}): {p['text']}" + (f" Also asked as: {p['aliases']}" if p.get("aliases") and (p["kind"] != "field") else "") for p in ps]
                P = model.encode(texts, normalize_embeddings=True, batch_size=32)
                Q = model.encode([qp + (r["q"] or " ") for r in rows], normalize_embeddings=True, batch_size=32)
                S = Q @ P.T
                idx = {p["source"]: i for i, p in enumerate(ps) if p["kind"] == "field"}
                ranked, conf = {}, {}
                for qi, r in enumerate(rows):
                    order = list(np.argsort(-S[qi], kind="stable"))
                    rk = base.dedupe(order, ps, S[qi])
                    c = float(S[qi][order[0]]) if r["q"] else -1.0
                    nm = base.named_fields(r["q"], labels)
                    if nm:
                        rk = ([{"i": idx[s], "source": s, "score": 1.0} for s in nm if s in idx] + [h for h in rk if h["source"] not in nm])[:TOPN]
                        c = 1.0
                    elif rk and rk[0]["source"].startswith("out of scope"):
                        c = -1.0                                          # closest to an out-of-scope topic: refuse
                    ranked[r["id"]] = [h for h in rk if not h["source"].startswith("out of scope")] or rk
                    conf[r["id"]] = c
                t, j = base.tune(dev, ranked, conf, grid)
                key = f"{name}{'+phrases' if phrases else ''}{'+decoys' if decoys else ''}"
                rep = {"threshold": t, "dev_objective": round(j, 3), "slices": {}}
                sl = {"answerable (20)": [r for r in test if "want" in r], "ar rules+fields (14)": [r for r in test if "want" in r and r["lang"] == "ar"],
                      "fr rules (6)": [r for r in test if "want" in r and r["lang"] == "fr"], "near-domain refused (20)": [r for r in test if "want" not in r and r["kind"] == "near-domain"],
                      "pure refused (4)": [r for r in test if "want" not in r and r["kind"] == "pure"],
                      "near-domain en (12)": [r for r in test if "want" not in r and r["kind"] == "near-domain" and r["lang"] == "en"],
                      "near-domain ar/fr (8)": [r for r in test if "want" not in r and r["kind"] == "near-domain" and r["lang"] != "en"]}
                for s, rs in sl.items():
                    m = base.measure(rs, ranked, conf, t)
                    if "hit1" in m:
                        n = len(m["hit1"])
                        r3 = [any(h["source"] in r["want"] for h in ranked[r["id"]][:3]) for r in rs]
                        rep["slices"][s] = {"n": n, "hit1": base.rate(m["hit1"]), "hit3": base.rate(m["hit3"]), "rank3": base.rate(r3), "hit3_ci": base.wilson(sum(m["hit3"]), n)}
                    else:
                        n = len(m["refused_off"])
                        rep["slices"][s] = {"n": n, "refused": base.rate(m["refused_off"]), "refused_ci": base.wilson(sum(m["refused_off"]), n)}
                pos = [conf[r["id"]] for r in test if "want" in r]
                neg = [conf[r["id"]] for r in test if "want" not in r]
                rep["auroc"] = round(float(np.mean([(p > q) + 0.5 * (p == q) for p in pos for q in neg])), 3)
                out[key] = rep
                print(f"[{key}] t={t} dev={j:.3f}", flush=True)
        del model
        torch.cuda.empty_cache()
    (HERE / "results_v3.json").write_text(json.dumps(out, indent=1, default=float), encoding="utf-8")
    print("\nTEST (v3, frozen before the features were written)")
    print(f"{'system':32} {'thr':>5} {'ans hit@3':>10} {'ar hit@3':>9} {'fr hit@3':>9} {'near refused':>13} {'pure':>5} {'AUROC':>6}")
    pct = lambda x: f"{100 * x:3.0f}%"  # noqa: E731
    for k, r in out.items():
        s = r["slices"]
        print(f"{k:32} {r['threshold']:>5} {pct(s['answerable (20)']['hit3']):>10} {pct(s['ar rules+fields (14)']['hit3']):>9} {pct(s['fr rules (6)']['hit3']):>9} {pct(s['near-domain refused (20)']['refused']):>13} {pct(s['pure refused (4)']['refused']):>5} {r['auroc']:>6}")


if __name__ == "__main__":
    main()
