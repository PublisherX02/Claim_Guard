"""Draws outputs/defense/auto_modes_graph.png from outputs/defense/auto_modes_run.json (docs/50, the real-pipeline run of each automatic step).

    python rust/tools/auto_modes_graph.py
"""
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

root = Path(__file__).resolve().parents[2]
d = json.loads((root / "outputs/defense/auto_modes_run.json").read_text(encoding="utf-8"))
size = d["batch_size"]
sc = d["scenarios"]
m = lambda s, k: s[k]["mean"]

GREY, GREEN, BLUE, INK = "#8b94a3", "#2e9e6a", "#3b82c4", "#1f2430"
labels = ["Today", "Auto-clear\nonly", "+ auto-request\n(medium severity)", "+ auto-request\n(all severities)"]
person = [size] + [m(s, "claims_a_person_still_handles") for s in sc]
cleared = [0] + [m(s, "claims_cleared_by_the_system") for s in sc]
asked = [0] + [m(s, "claims_the_system_requested_information_for") for s in sc]
no_person_total = [0] + [m(s, "claims_decided_without_a_person") for s in sc]
findings = [m(sc[0], "findings_in_the_batch")] + [m(s, "findings_a_person_still_decides") for s in sc]

fig, (a1, a2) = plt.subplots(1, 2, figsize=(12, 5), dpi=160, gridspec_kw={"width_ratios": [1.45, 1]})
y = list(range(len(labels)))[::-1]
a1.barh(y, person, color=GREY, height=0.55, label="a person handles")
a1.barh(y, cleared, left=person, color=GREEN, height=0.55, label="cleared by the system")
a1.barh(y, asked, left=[p + c for p, c in zip(person, cleared)], color=BLUE, height=0.55, label="system recorded the information request")
for yi, p, c, a, total in zip(y, person, cleared, asked, no_person_total):
    a1.text(p / 2, yi, f"{p:.0f}", ha="center", va="center", color="white", fontsize=10, fontweight="bold")
    if c:
        a1.text(p + c / 2, yi, f"{c:.0f}", ha="center", va="center", color="white", fontsize=10, fontweight="bold")
    if a >= 8:
        a1.text(p + c + a / 2, yi, f"{a:.0f}", ha="center", va="center", color="white", fontsize=9, fontweight="bold")
    if total:
        a1.text(size + 6, yi, f"{total:.0f} ({100 * total / size:.0f}%)\nwithout a person", ha="left", va="center", fontsize=8.5, color=INK)
a1.set_yticks(y)
a1.set_yticklabels(labels, fontsize=9.5)
a1.set_xlim(0, size * 1.3)
a1.set_xlabel("claims in a random batch of 500", color=INK)
a1.set_title("Claims a person still handles", loc="left", fontsize=11.5, fontweight="bold", color=INK)
a1.legend(loc="upper center", bbox_to_anchor=(0.45, -0.18), ncol=3, fontsize=7.5, frameon=False)

a2.barh(y, findings, color=[GREY, GREY, BLUE, BLUE], height=0.55)
for yi, f in zip(y, findings):
    pct = 100 * (findings[0] - f) / findings[0]
    a2.text(f + 8, yi, f"{f:.0f}" + (f"  (-{pct:.0f}%)" if pct > 0.5 else ""), va="center", fontsize=9.5, color=INK, fontweight="bold")
a2.set_yticks(y)
a2.set_yticklabels([])
a2.set_xlim(0, findings[0] * 1.3)
a2.set_xlabel("findings a person decides, per 500 claims", color=INK)
a2.set_title("Findings a person decides", loc="left", fontsize=11.5, fontweight="bold", color=INK)
for ax in (a1, a2):
    for side in ("top", "right", "left"):
        ax.spines[side].set_visible(False)
    ax.tick_params(axis="y", length=0)
fig.suptitle("What the real pipeline takes off a person's desk", x=0.02, ha="left", fontsize=14, fontweight="bold", color=INK)
fig.text(0.02, 0.012,
         f"Real engine and pipeline on the 600 labelled claims, the same {d['batches']} random batches of {size} in every row, {d['audit_percent']}% random audit. 0 cleared claims and 0 recorded requests disagreed with the answer key.\n"
         "Synthetic mix from the organizers, not a hospital's. 'Information request' is a recorded decision; nothing is sent to a provider. No review time measured. Off by default; waiting for the mentor (docs/50).",
         fontsize=7, color="#5a6272", va="bottom")
fig.subplots_adjust(left=0.13, right=0.97, top=0.86, bottom=0.27, wspace=0.08)
out = root / "outputs/defense/auto_modes_graph.png"
fig.savefig(out)
print("wrote", out)
