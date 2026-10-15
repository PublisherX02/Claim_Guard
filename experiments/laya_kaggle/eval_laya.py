"""Evaluate a Laya checkpoint (zero-shot base or fine-tuned) on the ClaimGuard test and stress splits.

    python eval_laya.py MODEL_DIR DATASET_DIR OUT_JSON [--device cuda] [--limit N]

Reports accuracy, macro-F1, per-class precision/recall, the confusion matrix, ECE, the majority-class baseline, and
an "auto-accept" table: if only answers whose calibrated confidence is at least t were accepted without a person,
how many cases would that cover, and how many of the accepted ones are wrong (and how many real FAILs would be
let through as PASS). The labels are the organizers' rule results, not reviewer decisions.
"""
import argparse
import json
import time
from collections import Counter
from pathlib import Path

import numpy as np

STATUSES = ["PASS", "FAIL", "UNABLE_TO_ASSESS", "NOT_APPLICABLE"]


def ece(conf, correct, bins=10):
    conf, correct = np.asarray(conf), np.asarray(correct, dtype=float)
    if len(conf) == 0:
        return float("nan")
    edges = np.linspace(0.0, 1.0, bins + 1)
    total = 0.0
    for lo, hi in zip(edges[:-1], edges[1:]):
        m = (conf > lo) & (conf <= hi) if lo > 0 else (conf >= lo) & (conf <= hi)
        if m.any():
            total += m.mean() * abs(correct[m].mean() - conf[m].mean())
    return float(total)


def metrics(rows):
    gold = [r["gold"] for r in rows]
    pred = [r["pred"] for r in rows]
    conf = np.array([r["conf"] for r in rows])
    ok = np.array([g == p for g, p in zip(gold, pred)])
    per = {}
    for s in STATUSES:
        tp = sum(1 for g, p in zip(gold, pred) if g == s and p == s)
        fp = sum(1 for g, p in zip(gold, pred) if g != s and p == s)
        fn = sum(1 for g, p in zip(gold, pred) if g == s and p != s)
        prec = tp / (tp + fp) if tp + fp else 0.0
        rec = tp / (tp + fn) if tp + fn else 0.0
        f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
        per[s] = {"support": sum(1 for g in gold if g == s), "precision": round(prec, 4), "recall": round(rec, 4), "f1": round(f1, 4)}
    confusion = {g: {p: sum(1 for a, b in zip(gold, pred) if a == g and b == p) for p in STATUSES} for g in STATUSES}
    major = Counter(gold).most_common(1)[0]
    table = []
    for t in [0.0, 0.5, 0.7, 0.8, 0.9, 0.95, 0.99]:
        m = conf >= t
        n = int(m.sum())
        wrong = int((~ok[m]).sum())
        fail_as_pass = sum(1 for g, p, k in zip(gold, pred, m) if k and g == "FAIL" and p == "PASS")
        table.append({"min_confidence": t, "accepted": n, "share_of_cases": round(n / len(rows), 4),
                      "wrong_among_accepted": wrong, "accuracy_among_accepted": round(1 - wrong / n, 4) if n else None,
                      "real_FAILs_accepted_as_PASS": fail_as_pass})
    return {"cases": len(rows), "accuracy": round(float(ok.mean()), 4),
            "macro_f1": round(float(np.mean([per[s]["f1"] for s in STATUSES])), 4),
            "majority_class": {"label": major[0], "accuracy": round(major[1] / len(rows), 4)},
            "ece": round(ece(conf, ok), 4), "mean_confidence": round(float(conf.mean()), 4),
            "per_class": per, "confusion_gold_by_pred": confusion, "auto_accept_if_confidence_at_least": table}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("model_dir")
    ap.add_argument("dataset_dir")
    ap.add_argument("out_json")
    ap.add_argument("--device", default="cuda")
    ap.add_argument("--limit", type=int, default=0, help="evaluate only the first N cases per split (smoke test)")
    a = ap.parse_args()

    import laya
    agent = laya.Agent(a.model_dir, device=a.device)
    if getattr(agent, "cfg", None) is not None:
        agent.cfg["max_len"] = max(agent.cfg.get("max_len", 512), 1024)  # a claim plus its rule is ~600-900 tokens
        agent.cfg["head_max_len"] = max(agent.cfg.get("head_max_len", 192), 256)

    report = {"model": a.model_dir, "note": "labels are the organizers' rule results on real labelled claims, not reviewer decisions"}
    for split in ["test", "stress"]:
        cases = [json.loads(l) for l in Path(a.dataset_dir, f"{split}.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()]
        if a.limit:
            cases = cases[: a.limit]
        rows, lat = [], []
        t0 = time.time()
        for c in cases:
            t = time.perf_counter()
            res = agent.predict(c["state"], c["questions"])["answers"]["status"]
            lat.append((time.perf_counter() - t) * 1000)
            rows.append({"id": c["id"], "rule_id": c["rule_id"], "gold": c["gold"]["status"]["label"], "pred": res["choice"],
                         "conf": float(res.get("answer_confidence", res.get("confidence", 0.0))), "probabilities": res["probabilities"]})
        report[split] = metrics(rows)
        report[split]["latency_ms_p50"] = round(float(np.percentile(lat, 50)), 1)
        by_rule = {}
        for r in rows:
            by_rule.setdefault(r["rule_id"], []).append(r["gold"] == r["pred"])
        report[split]["accuracy_by_rule"] = {k: round(float(np.mean(v)), 4) for k, v in sorted(by_rule.items())}
        Path(a.out_json).with_suffix(f".{split}.predictions.jsonl").write_text("\n".join(json.dumps(r) for r in rows), encoding="utf-8")
        s = report[split]
        print(f"{split}: {len(rows)} cases in {time.time() - t0:.0f}s | acc {s['accuracy']} macroF1 {s['macro_f1']} "
              f"majority {s['majority_class']['accuracy']} ECE {s['ece']}", flush=True)
    Path(a.out_json).write_text(json.dumps(report, indent=2), encoding="utf-8")
    print("wrote", a.out_json)


if __name__ == "__main__":
    main()
