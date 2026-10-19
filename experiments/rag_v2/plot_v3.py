"""Graphs for the second round (results_v3.json): the two upgrades and the model change on the frozen v3 test set."""
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

HERE = Path(__file__).resolve().parent
OUT = HERE.parents[1] / "docs" / "img" / "rag_v2"
R = json.loads((HERE / "results_v3.json").read_text(encoding="utf-8"))
names = list(R)
x = np.arange(len(names))

fig, ax = plt.subplots(1, 3, figsize=(17, 4.8))
for k, (title, get) in zip(ax, [
        ("Answerable questions found in the top 3 (n=20)", lambda r: 100 * r["slices"]["answerable (20)"]["hit3"]),
        ("Near-domain questions refused (n=20)", lambda r: 100 * r["slices"]["near-domain refused (20)"]["refused"]),
        ("AUROC (answerable vs off-topic)", lambda r: r["auroc"])]):
    vals = [get(R[n]) for n in names]
    k.bar(x, vals, color=["tab:blue" if n.startswith("minilm") else "tab:green" for n in names])
    for i, v in enumerate(vals):
        k.text(i, v, f"{v:.0f}" if "AUROC" not in title else f"{v:.2f}", ha="center", va="bottom", fontsize=8)
    k.set_title(title, fontsize=10)
    k.set_xticks(x)
    k.set_xticklabels([n.replace("minilm_multi", "MiniLM").replace("bge_m3", "bge-m3") for n in names], rotation=40, ha="right", fontsize=8)
plt.suptitle("v3 test set (frozen before the features): blue = MiniLM (shipped before), green = bge-m3", fontsize=11)
plt.tight_layout()
plt.savefig(OUT / "5_round2_models_and_features.png", dpi=130)
plt.close()

fig, ax = plt.subplots(figsize=(8.5, 4.8))
groups = ["ar rules+fields (14)", "fr rules (6)"]
for i, n in enumerate(["minilm_multi", "bge_m3", "bge_m3+phrases", "bge_m3+phrases+decoys"]):
    ax.bar(np.arange(len(groups)) + i * 0.2, [100 * R[n]["slices"][g]["hit3"] for g in groups], 0.2, label=n)
ax.set_xticks(np.arange(len(groups)) + 0.3)
ax.set_xticklabels(["Arabic (n=14)", "French (n=6)"])
ax.set_ylabel("hit@3 %")
ax.set_title("Arabic and French questions, v3 test")
ax.legend(fontsize=8)
plt.tight_layout()
plt.savefig(OUT / "6_round2_arabic_french.png", dpi=130)
print("ok")
