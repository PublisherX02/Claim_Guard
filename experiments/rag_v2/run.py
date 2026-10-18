"""RAG rebuild experiment (docs/52 follow-up): which embedding model, with or without Arabic field aliases, with or without a cross-encoder gate,
retrieves best over the rulebook AND the field dictionary, in English, Arabic and French, and refuses best what the rulebook cannot answer?

    python experiments/rag_v2/run.py [--models name,name] [--no-rerank]

Corpus: the 111 passages of rag.rs plus one passage per claim field (the data dictionary, `field_glossary.json`).
Questions: `questions_v2.json` (frozen before any candidate ran), plus docs/52's paraphrase set and the original 26 of rag.rs.
Thresholds are chosen on 'dev' rows only (objective: half hit@3 on the answerable rows, half the mean of near-domain and pure off-topic refusal, so refusing everything or
nothing both score 0.5 at best); every number quoted is on 'test' with a 95% Wilson interval. Nothing is generated; the answer stays a quoted passage.
"""
import argparse
import json
import math
import sys
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "experiments" / "rag_semantic"))
from bm25_port import load_corpus  # noqa: E402

GLOSSARY = ROOT / "rust" / "crates" / "cg-core" / "data" / "field_glossary.json"
TOPN = 5
MODELS = {
    "minilm_multi": ("sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2", "", ""),
    "e5_small": ("intfloat/multilingual-e5-small", "query: ", "passage: "),
    "e5_base": ("intfloat/multilingual-e5-base", "query: ", "passage: "),
    "bge_m3": ("BAAI/bge-m3", "", ""),
}
RERANKERS = {"mmarco": "cross-encoder/mmarco-mMiniLMv2-L12-H384-v1", "bge_reranker": "BAAI/bge-reranker-v2-m3"}


def norm_label(s):
    out, gap = [], True
    for c in s:
        o = ord(c)
        if 0x64B <= o <= 0x65F or o in (0x670, 0x640) or 0x200B <= o <= 0x200F:
            continue
        c = {"أ": "ا", "إ": "ا", "آ": "ا", "ٱ": "ا", "ى": "ي", "ة": "ه"}.get(c, c)
        if c.isspace() or c in "_-:,،.؟?!/()[]؛;":
            if not gap:
                out.append(" ")
                gap = True
            continue
        out.append(c.lower())
        gap = False
    return "".join(out).strip()


CUES = ["ما هو", "ما هي", "ماذا", "المقصود", "معني", "يعني", "تعريف", "حقل", "فائده"]


def named_fields(q, labels):
    """Mirror of cg_core::localize::fields_named: Arabic dictionary lookup, only for 'what is / what does it mean' questions."""
    t = f" {norm_label(q)} "
    if not any("؀" <= c <= "ۿ" for c in t) or not any(c in t for c in CUES):
        return []
    found = []
    for si, (label, src) in enumerate(labels):
        if f" {label} " in t or any(f" {c}{label} " in t for c in "بلوفك"):
            found.append((len(label), si, src))
    found.sort(key=lambda x: (-x[0], x[1], x[2]))
    out = []
    for ln, _, src in found:
        if ln * 2 < found[0][0] or src in out:
            continue
        out.append(src)
    return out[:3]


def field_labels():
    g = json.loads(GLOSSARY.read_text(encoding="utf-8"))
    out = []
    for scope, prefix in [("claim", ""), ("coverage", "coverage."), ("line", "lines[]."), ("authorization", "authorizations[]."), ("attachment", "attachments[].")]:
        for key, f in g["scopes"][scope].items():
            for a in f.get("ar", []):
                out.append((norm_label(a), f"field {prefix}{key}"))
    return out


def field_passages():
    g = json.loads(GLOSSARY.read_text(encoding="utf-8"))
    out = []
    for scope, place, prefix in [("claim", "claim", ""), ("coverage", "coverage record of a claim", "coverage."), ("line", "service line of a claim", "lines[]."),
                                 ("authorization", "authorization record of a claim", "authorizations[]."), ("attachment", "attachment record of a claim", "attachments[].")]:
        for key, f in g["scopes"][scope].items():
            used = f.get("used_by") or []
            used_text = f"Used by {', '.join(used)}." if used else "No rule reads it."
            text = f"Field {prefix}{key} ({place}; {f.get('type', '')}): {f['meaning']} {used_text}"
            out.append({"source": f"field {prefix}{key}", "kind": "field", "text": text, "aliases": "، ".join(f.get("ar", []))})
    return out


def wilson(k, n, z=1.96):
    if n == 0:
        return (None, None)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - h) / d, (c + h) / d)


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


def sigmoid(x):
    return 1.0 / (1.0 + np.exp(-x))


def measure(rows, ranked, conf, t):
    """Per-row outcomes at threshold t: for answerable rows (hit1, hit3, refused); for off-topic rows (refused)."""
    ans = [r for r in rows if "want" in r]
    off = [r for r in rows if "want" not in r]
    m = {}
    if ans:
        m["hit1"] = [conf[r["id"]] >= t and ranked[r["id"]][0]["source"] in r["want"] for r in ans]
        m["hit3"] = [conf[r["id"]] >= t and any(h["source"] in r["want"] for h in ranked[r["id"]][:3]) for r in ans]
        m["refused_ans"] = [conf[r["id"]] < t for r in ans]
    if off:
        m["refused_off"] = [conf[r["id"]] < t for r in off]
    return m


def rate(xs):
    return float(np.mean(xs)) if len(xs) else None


def objective(dev, ranked, conf, t):
    near = [r for r in dev if "want" not in r and r["kind"] == "near-domain"]
    pure = [r for r in dev if "want" not in r and r["kind"] == "pure"]
    ans = [r for r in dev if "want" in r]
    hit = rate(measure(ans, ranked, conf, t)["hit3"])
    off = [rate(measure(near, ranked, conf, t)["refused_off"]), rate(measure(pure, ranked, conf, t)["refused_off"])]
    return 0.5 * hit + 0.5 * float(np.mean([x for x in off if x is not None]))


def tune(dev, ranked, conf, grid):
    best = None
    for t in grid:
        j = objective(dev, ranked, conf, t)
        if best is None or j > best[0] + 1e-9:
            best = (j, float(t))
    return best[1], best[0]


def load_rows():
    v2 = json.loads((HERE / "questions_v2.json").read_text(encoding="utf-8"))
    para = json.loads((ROOT / "experiments" / "rag_semantic" / "questions_paraphrase.json").read_text(encoding="utf-8"))
    rust = json.loads((ROOT / "experiments" / "rag_semantic" / "questions_rust26.json").read_text(encoding="utf-8"))
    rows = []
    for x in v2["answerable"]:
        rows.append({**x, "set": "v2"})
    for x in v2["off_topic"]:
        rows.append({**x, "set": "v2"})
    for x in para["answerable"]:
        rows.append({**x, "set": "para", "topic": "rule"})
    for x in para["off_topic"]:
        rows.append({"id": x["id"], "q": x["q"], "split": x["split"], "set": "para", "kind": x["kind"], "lang": "en"})
    for i, x in enumerate(rust["answerable"]):
        rows.append({"id": f"R{i + 1:02d}", "q": x["q"], "want": x["want"], "split": "rust", "set": "rust", "lang": "en", "topic": "rule"})
    for i, q in enumerate(rust["off_topic"]):
        rows.append({"id": f"RO{i + 1:02d}", "q": q, "split": "rust", "set": "rust", "kind": "pure", "lang": "en"})
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default=",".join(MODELS))
    ap.add_argument("--rerankers", default=",".join(RERANKERS))
    ap.add_argument("--no-rerank", action="store_true")
    a = ap.parse_args()
    import torch
    from sentence_transformers import CrossEncoder, SentenceTransformer

    ps = load_corpus() + field_passages()
    rows = load_rows()
    print(f"corpus {len(ps)} passages ({sum(p['kind'] == 'field' for p in ps)} field), {len(rows)} questions", flush=True)
    grid = np.round(np.arange(0.0, 1.0, 0.005), 3)
    systems, info = {}, {}
    cand = {}                                                                     # dense top-5 candidates for the reranker, per dense system

    for name in a.models.split(","):
        mid, qp, pp = MODELS[name]
        try:
            model = SentenceTransformer(mid, device="cuda")
        except Exception as e:  # noqa: BLE001
            print("SKIP", name, repr(e)[:200], flush=True)
            continue
        model.max_seq_length = 256
        if torch.cuda.is_available():
            model.half()
        for alias in (False, True):
            t0 = time.time()
            texts = [pp + f"{p['source']} ({p['kind']}): {p['text']}" + (f" Arabic names: {p['aliases']}" if alias and p.get("aliases") else "") for p in ps]
            P = model.encode(texts, normalize_embeddings=True, batch_size=32)
            Q = model.encode([qp + (r["q"] or " ") for r in rows], normalize_embeddings=True, batch_size=32)
            S = Q @ P.T
            ranked, conf = {}, {}
            for qi, r in enumerate(rows):
                order = list(np.argsort(-S[qi], kind="stable"))
                ranked[r["id"]] = dedupe(order, ps, S[qi])
                conf[r["id"]] = float(S[qi][order[0]]) if r["q"] else -1.0
            key = f"dense_{name}" + ("+alias" if alias else "")
            systems[key] = (ranked, conf)
            cand[key] = ranked
            info[key] = {"seconds": round(time.time() - t0, 1), "dim": int(P.shape[1])}
            print(f"[{key}] {time.time() - t0:.1f}s", flush=True)
        del model
        torch.cuda.empty_cache()

    labels = field_labels()
    idx = {p["source"]: i for i, p in enumerate(ps) if p["kind"] == "field"}
    for key in list(systems):
        if not key.startswith("dense_"):
            continue
        ranked, conf = {}, {}
        for r in rows:
            nm = named_fields(r["q"], labels)
            base = systems[key][0][r["id"]]
            head = [{"i": idx[s_], "source": s_, "score": 1.0} for s_ in nm if s_ in idx]
            ranked[r["id"]] = (head + [h for h in base if h["source"] not in nm])[:TOPN]
            conf[r["id"]] = 1.0 if head else systems[key][1][r["id"]]
        systems[key + "+lookup"] = (ranked, conf)

    if not a.no_rerank and cand:
        base_key = max(cand, key=lambda k: 0)                                    # placeholder, replaced below by the best dev system
        dev = [r for r in rows if r["split"] == "dev"]
        scored = {k: tune(dev, *systems[k], grid)[1] for k in cand}
        base_key = max(scored, key=scored.get)
        info["reranker_candidates_from"] = base_key
        print("reranking the top five of", base_key, flush=True)
        for rn in a.rerankers.split(","):
            try:
                ce = CrossEncoder(RERANKERS[rn], device="cuda", max_length=384)
            except Exception as e:  # noqa: BLE001
                print("SKIP reranker", rn, repr(e)[:200], flush=True)
                continue
            ranked, conf = {}, {}
            t0 = time.time()
            for r in rows:
                cs = cand[base_key][r["id"]]
                if not r["q"]:
                    ranked[r["id"]], conf[r["id"]] = cs, -1.0
                    continue
                s = sigmoid(np.asarray(ce.predict([(r["q"], ps[c["i"]]["text"]) for c in cs], batch_size=8, show_progress_bar=False), dtype=float))
                order = sorted(range(len(cs)), key=lambda j: (-s[j], j))
                ranked[r["id"]] = [{**cs[j], "score": float(s[j])} for j in order]
                conf[r["id"]] = float(s[order[0]])
            systems[f"rerank_{rn}"] = (ranked, conf)
            info[f"rerank_{rn}"] = {"seconds": round(time.time() - t0, 1)}
            print(f"[rerank_{rn}] {time.time() - t0:.1f}s", flush=True)
            del ce
            torch.cuda.empty_cache()

    dev = [r for r in rows if r["split"] == "dev"]
    test = [r for r in rows if r["split"] == "test"]
    slices = {
        "test: answerable (all)": [r for r in test if "want" in r],
        "test: answerable en": [r for r in test if "want" in r and r["lang"] == "en"],
        "test: answerable ar": [r for r in test if "want" in r and r["lang"] == "ar"],
        "test: answerable fr": [r for r in test if "want" in r and r["lang"] == "fr"],
        "test: field questions": [r for r in test if "want" in r and r.get("topic") == "field"],
        "test: field questions ar": [r for r in test if "want" in r and r.get("topic") == "field" and r["lang"] == "ar"],
        "test: rule questions": [r for r in test if "want" in r and r.get("topic") == "rule"],
        "test: near-domain refused": [r for r in test if "want" not in r and r["kind"] == "near-domain"],
        "test: pure off-topic refused": [r for r in test if "want" not in r and r["kind"] == "pure"],
        "original 26 (answerable)": [r for r in rows if r["set"] == "rust" and "want" in r],
        "original off-topic (5)": [r for r in rows if r["set"] == "rust" and "want" not in r],
    }
    report = {"corpus": len(ps), "info": info, "systems": {}}
    for name, (ranked, conf) in systems.items():
        t, j = tune(dev, ranked, conf, grid)
        rep = {"threshold": t, "dev_objective": round(j, 3), "slices": {}}
        for s, rs in slices.items():
            m = measure(rs, ranked, conf, t)
            if "hit1" in m:
                n = len(m["hit1"])
                ans = [r for r in rs if "want" in r]
                rank1 = [ranked[r["id"]][0]["source"] in r["want"] for r in ans]
                rank3 = [any(h["source"] in r["want"] for h in ranked[r["id"]][:3]) for r in ans]
                rep["slices"][s] = {"n": n, "hit1": rate(m["hit1"]), "hit3": rate(m["hit3"]), "refused": rate(m["refused_ans"]), "hit3_ci": wilson(sum(m["hit3"]), n),
                                    "rank1": rate(rank1), "rank3": rate(rank3)}
            else:
                n = len(m["refused_off"])
                rep["slices"][s] = {"n": n, "refused": rate(m["refused_off"]), "refused_ci": wilson(sum(m["refused_off"]), n)}
        pos = [conf[r["id"]] for r in test if "want" in r]
        for tag, kinds in (("all off-topic", ("near-domain", "pure")), ("near-domain only", ("near-domain",))):
            neg = [conf[r["id"]] for r in test if "want" not in r and r["kind"] in kinds]
            rep[f"auroc {tag}"] = round(float(np.mean([(p > q) + 0.5 * (p == q) for p in pos for q in neg])), 3)
        report["systems"][name] = rep
    (HERE / "results.json").write_text(json.dumps(report, indent=1, default=float), encoding="utf-8")

    def pct(x):
        return "  - " if x is None else f"{100 * x:3.0f}%"
    print("\nthreshold-free: ranking hit@1/hit@3 of answerable test rows, and AUROC of the confidence (answerable vs off-topic)")
    for name, rep in report["systems"].items():
        a_ = rep["slices"]["test: answerable (all)"]
        print(f"   {name:34} rank hit@1 {pct(a_['rank1'])} hit@3 {pct(a_['rank3'])}   AUROC all {rep['auroc all off-topic']:.3f}  near-domain {rep['auroc near-domain only']:.3f}")
    print("\nthreshold from dev; test numbers; hit@k counts a refusal as a miss\n")
    for s in slices:
        print(f"== {s}  (n = {next(iter(report['systems'].values()))['slices'][s]['n']})")
        for name, rep in report["systems"].items():
            m = rep["slices"][s]
            if "hit3" in m:
                lo, hi = m["hit3_ci"]
                print(f"   {name:34} t={rep['threshold']:<5} hit@1 {pct(m['hit1'])} hit@3 {pct(m['hit3'])} [{100 * lo:.0f}-{100 * hi:.0f}]  refused {pct(m['refused'])}")
            else:
                lo, hi = m["refused_ci"]
                print(f"   {name:34} t={rep['threshold']:<5} refused {pct(m['refused'])} [{100 * lo:.0f}-{100 * hi:.0f}]")
        print()


if __name__ == "__main__":
    main()
