"""Draws outputs/defense/auto_clear_graph.png from outputs/defense/auto_clear_run.json (the real-pipeline run of docs/50).

    python rust/tools/auto_clear_graph.py
"""
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

root = Path(__file__).resolve().parents[2]
d = json.loads((root / "outputs/defense/auto_clear_run.json").read_text(encoding="utf-8"))
size = d["batch_size"]
cleared = d["cleared_by_the_system"]["mean"]
audited = d["green_claims_sent_to_a_person_as_the_audit_sample"]["mean"]
flagged = d["claims_with_findings_left_for_a_person"]["mean"]
lo, hi = d["cleared_by_the_system"]["min"], d["cleared_by_the_system"]["max"]
wrong = d["cleared_but_the_answer_key_has_a_finding"]["max"]

GREEN, BLUE, GREY, INK = "#2e9e6a", "#e0a43a", "#8b94a3", "#1f2430"
fig, ax = plt.subplots(figsize=(10, 4.4), dpi=160)
rows = ["Today\n(a person handles every claim)", "With auto-clear on\n(5% random audit)"]
ypos = [1, 0]

# today: all 500 for a person
ax.barh(ypos[0], size, color=GREY, height=0.5)
ax.text(size / 2, ypos[0], f"{size} claims handled by a person", ha="center", va="center", color="white", fontsize=11, fontweight="bold")

# with auto-clear: flagged + audited for a person, cleared by the system
ax.barh(ypos[1], flagged, color=GREY, height=0.5)
ax.barh(ypos[1], audited, left=flagged, color=BLUE, height=0.5)
ax.barh(ypos[1], cleared, left=flagged + audited, color=GREEN, height=0.5)
ax.text(flagged / 2, ypos[1], f"{flagged:.0f} with findings\n(a person decides)", ha="center", va="center", color="white", fontsize=10, fontweight="bold")
ax.text(flagged + audited / 2, ypos[1] - 0.42, f"{audited:.1f} audit sample\n(a person checks)", ha="center", va="top", color=INK, fontsize=9)
ax.text(flagged + audited + cleared / 2, ypos[1], f"{cleared:.0f} cleared by\nthe system", ha="center", va="center", color="white", fontsize=11, fontweight="bold")

ax.set_yticks(ypos)
ax.set_yticklabels(rows, fontsize=10)
ax.set_xlim(0, size)
ax.set_ylim(-1.0, 1.55)
ax.set_xlabel("claims in a random batch of 500", color=INK)
for side in ("top", "right", "left"):
    ax.spines[side].set_visible(False)
ax.tick_params(axis="y", length=0)
ax.set_title(f"The real pipeline clears {cleared:.0f} of {size} claims on its own ({100 * cleared / size:.1f}%)", loc="left", fontsize=13, fontweight="bold", color=INK, pad=14)
fig.text(0.02, 0.015,
         f"Real engine and pipeline on the 600 labelled claims, {d['batches']} random batches of {size}. Cleared claims per batch ranged {lo:.0f} to {hi:.0f}; "
         f"{wrong:.0f} had a finding in the answer key.\nSynthetic mix from the organizers, so this is what the policy does on this data, not a hospital's share. No review time measured. (docs/50)",
         fontsize=7.5, color="#5a6272", va="bottom")
fig.subplots_adjust(left=0.27, right=0.97, top=0.86, bottom=0.2)
out = root / "outputs/defense/auto_clear_graph.png"
fig.savefig(out)
print("wrote", out)
