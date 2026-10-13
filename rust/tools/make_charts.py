"""Charts for the OCR reader comparison and the rulebook retrieval. The numbers are the ones recorded in docs/45 (doc_eval.py on 240 synthetic documents;
rag.rs tests on 26 questions); update them here when those are re-measured.

    .venv/Scripts/python.exe rust/tools/make_charts.py
"""
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

OUT = Path(__file__).resolve().parents[2] / 'docs' / 'figures'
READERS = ['Tesseract\nalone', 'Tesseract\nupscaled', 'Two readers\n(Tesseract + vision)', 'Two readers,\nupscaled (default)']
FIELDS = [94.6, 95.5, 96.8, 97.4]
DOCS = [50.4, 57.9, 59.6, 66.7]
FLAGGED = [312, 237, 205, 167]
SILENT = [41, 56, 3, 1]
C = ['#8da0b3', '#8da0b3', '#2a7f62', '#2a7f62']


def bars(ax, vals, title, ylabel, ylim=None, fmt='%.1f%%'):
    b = ax.bar(READERS, vals, color=C)
    ax.set_title(title, fontsize=11)
    ax.set_ylabel(ylabel)
    if ylim:
        ax.set_ylim(*ylim)
    for r, v in zip(b, vals):
        ax.text(r.get_x() + r.get_width() / 2, v, fmt % v, ha='center', va='bottom', fontsize=9)
    ax.tick_params(axis='x', labelsize=8)
    ax.spines[['top', 'right']].set_visible(False)


fig, ax = plt.subplots(1, 2, figsize=(11, 4.2))
bars(ax[0], FIELDS, 'Fields read correctly', '% of fields', (90, 100))
bars(ax[1], DOCS, 'Documents with every field right', '% of documents', (0, 80))
fig.suptitle('OCR readers on 240 synthetic documents (about 40 fields each)', fontweight='bold')
fig.tight_layout()
fig.savefig(OUT / 'ocr_reader_accuracy.png', dpi=160)

fig, ax = plt.subplots(figsize=(8, 4.2))
x = range(len(READERS))
w = 0.38
ax.bar([i - w / 2 for i in x], SILENT, w, label='silent (wrong and not flagged)', color='#c0392b')
b = ax.bar([i + w / 2 for i in x], [f for f in FLAGGED], w, label='wrong and flagged for a person', color='#8da0b3')
for i, (s, f) in enumerate(zip(SILENT, FLAGGED)):
    ax.text(i - w / 2, s, str(s), ha='center', va='bottom', fontsize=9)
    ax.text(i + w / 2, f, str(f), ha='center', va='bottom', fontsize=9)
ax.set_xticks(list(x))
ax.set_xticklabels(READERS, fontsize=8)
ax.set_ylabel('wrong fields (of about 9,600)')
ax.set_title('Wrong fields: the second reader turns silent errors into flagged ones', fontsize=11)
ax.legend(frameon=False)
ax.spines[['top', 'right']].set_visible(False)
fig.tight_layout()
fig.savefig(OUT / 'ocr_silent_errors.png', dpi=160)

fig, ax = plt.subplots(figsize=(6.5, 4))
cond = ['PDF text\nlayer', 'Scan', 'Image-only\nPDF', 'Phone-style\nphoto']
vals = [100.0, 98.9, 98.7, 88.1]
b = ax.bar(cond, vals, color=['#2a7f62', '#2a7f62', '#2a7f62', '#d68910'])
for r, v in zip(b, vals):
    ax.text(r.get_x() + r.get_width() / 2, v, '%.1f%%' % v, ha='center', va='bottom', fontsize=9)
ax.set_ylim(80, 102)
ax.set_ylabel('% of fields right')
ax.set_title('Default pipeline by input condition', fontsize=11)
ax.spines[['top', 'right']].set_visible(False)
fig.tight_layout()
fig.savefig(OUT / 'ocr_by_condition.png', dpi=160)

fig, ax = plt.subplots(1, 2, figsize=(9, 3.8))
b = ax[0].bar(['hit@1', 'hit@3'], [88, 96], color=['#8da0b3', '#2a7f62'])
for r, v in zip(b, [88, 96]):
    ax[0].text(r.get_x() + r.get_width() / 2, v, '%d%%' % v, ha='center', va='bottom')
ax[0].set_ylim(0, 110)
ax[0].set_ylabel('% of 26 questions')
ax[0].set_title('Right rule or policy found', fontsize=11)
b = ax[1].bar(['Refused\n(off-topic or clinical)', 'Answered\n(should refuse)'], [5, 0], color=['#2a7f62', '#c0392b'])
for r, v in zip(b, [5, 0]):
    ax[1].text(r.get_x() + r.get_width() / 2, v, str(v), ha='center', va='bottom')
ax[1].set_ylim(0, 6)
ax[1].set_ylabel('of 5 unanswerable questions')
ax[1].set_title('Refusal', fontsize=11)
for a in ax:
    a.spines[['top', 'right']].set_visible(False)
fig.suptitle('Ask the rulebook (BM25 retrieval with citations)', fontweight='bold')
fig.tight_layout()
fig.savefig(OUT / 'rag_retrieval.png', dpi=160)
print('ok')
