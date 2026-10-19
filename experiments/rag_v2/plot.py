"""Graphs for docs/55 from results.json and per_row.json (run run.py first)."""
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

HERE = Path(__file__).resolve().parent
OUT = HERE.parents[1] / "docs" / "img" / "rag_v2"
OUT.mkdir(parents=True, exist_ok=True)
R = json.loads((HERE / "results.json").read_text(encoding="utf-8"))
PR = json.loads((HERE / "per_row.json").read_text(encoding="utf-8"))
S = R["systems"]
names = list(S)


def short(n):
    return n.replace("dense_", "").replace("rerank_", "rerank ")


# 1. threshold-free ranking and AUROC
fig, ax = plt.subplots(1, 2, figsize=(13, 5))
x = np.arange(len(names))
a = [S[n]["slices"]["test: answerable (all)"] for n in names]
ax[0].bar(x - 0.2, [100 * m["rank1"] for m in a], 0.4, label="hit@1")
ax[0].bar(x + 0.2, [100 * m["rank3"] for m in a], 0.4, label="hit@3")
ax[0].set_title("Ranking quality, test answerable rows (n=48), no threshold")
ax[0].set_ylabel("%")
ax[1].bar(x - 0.2, [S[n]["auroc all off-topic"] for n in names], 0.4, label="vs all off-topic")
ax[1].bar(x + 0.2, [S[n]["auroc near-domain only"] for n in names], 0.4, label="vs near-domain only")
ax[1].axhline(0.5, color="grey", ls=":")
ax[1].set_title("AUROC: can the confidence tell answerable from refusable?")
for k in ax:
    k.set_xticks(x)
    k.set_xticklabels([short(n) for n in names], rotation=40, ha="right", fontsize=8)
    k.legend()
plt.tight_layout()
plt.savefig(OUT / "1_ranking_and_auroc.png", dpi=130)
plt.close()

# 2. answer / refuse at the dev threshold, by slice
slices = ["test: answerable en", "test: answerable ar", "test: answerable fr", "test: field questions ar", "original 26 (answerable)"]
refs = ["test: near-domain refused", "test: pure off-topic refused"]
fig, ax = plt.subplots(1, 2, figsize=(14, 5))
w = 0.8 / len(names)
for i, n in enumerate(names):
    ax[0].bar(np.arange(len(slices)) + i * w, [100 * S[n]["slices"][s]["hit3"] for s in slices], w, label=short(n))
    ax[1].bar(np.arange(len(refs)) + i * w, [100 * S[n]["slices"][s]["refused"] for s in refs], w, label=short(n))
ax[0].set_xticks(np.arange(len(slices)) + 0.4)
ax[0].set_xticklabels([s.replace("test: ", "") for s in slices], rotation=20, ha="right", fontsize=8)
ax[0].set_title("hit@3 at the dev-chosen threshold (a refusal counts as a miss)")
ax[1].set_xticks(np.arange(len(refs)) + 0.4)
ax[1].set_xticklabels([s.replace("test: ", "") for s in refs])
ax[1].set_title("Refused correctly (higher is better)")
ax[1].legend(fontsize=7, ncol=2)
for k in ax:
    k.set_ylabel("%")
plt.tight_layout()
plt.savefig(OUT / "2_hit_and_refusal_at_threshold.png", dpi=130)
plt.close()

# 3. confidence distributions of the best systems
best = sorted(names, key=lambda n: -S[n]["auroc all off-topic"])[:4]
fig, ax = plt.subplots(1, len(best), figsize=(4.2 * len(best), 3.8))
for k, n in zip(np.atleast_1d(ax), best):
    rows = PR[n].values()
    for kind, c in (("answerable", "tab:green"), ("near-domain", "tab:orange"), ("pure", "tab:red")):
        v = [r["conf"] for r in rows if r["kind"] == kind and r["split"] == "test" and r["conf"] > -1]
        k.hist(v, bins=14, alpha=0.55, color=c, label=kind)
    k.axvline(S[n]["threshold"], color="k", ls="--")
    k.set_title(short(n), fontsize=9)
np.atleast_1d(ax)[0].legend(fontsize=7)
plt.suptitle("Top-passage score on test rows; dashed = threshold chosen on dev", fontsize=10)
plt.tight_layout()
plt.savefig(OUT / "3_score_distributions.png", dpi=130)
plt.close()

# 4. threshold sweep for the best system: hit@3 vs near-domain refusal
n = best[0]
rows = [r for r in PR[n].values() if r["split"] in ("dev", "test")]
ts = np.linspace(min(r["conf"] for r in rows if r["conf"] > -1), max(r["conf"] for r in rows), 80)
fig, ax = plt.subplots(figsize=(6.5, 4.5))
for kind, lab in (("answerable", "answers kept (top-1 right)"), ("near-domain", "near-domain refused"), ("pure", "off-topic refused")):
    rs = [r for r in rows if r["kind"] == kind]
    ys = [100 * np.mean([(r["conf"] < t) if kind != "answerable" else (r["conf"] >= t and r["right@1"]) for r in rs]) for t in ts]
    ax.plot(ts, ys, label=lab)
ax.axvline(S[n]["threshold"], color="k", ls="--")
ax.set_title(f"Threshold trade-off, {short(n)} (dev+test rows)")
ax.set_xlabel("threshold")
ax.set_ylabel("%")
ax.legend()
plt.tight_layout()
plt.savefig(OUT / "4_threshold_tradeoff.png", dpi=130)
plt.close()
print("wrote", sorted(p.name for p in OUT.iterdir()))
