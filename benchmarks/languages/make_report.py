"""Turn results/results.json into the figures and tables of docs/36_Language_Choice.md.

    python benchmarks/languages/make_report.py

Measured numbers come from results.json only. The two judged inputs (how much a language's design prevents bugs, and how well it fits
this project's delivery constraints) live in judged.json with a written reason for every score, so they can be argued with and changed;
the weighted ranking is recomputed from whatever is in that file.
"""
import json, math
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FIG = ROOT / 'docs' / 'figures' / 'languages'
FIG.mkdir(parents=True, exist_ok=True)
R = json.loads((HERE / 'results' / 'results.json').read_text())
J = json.loads((HERE / 'judged.json').read_text())
ORDER = ['rust', 'go', 'csharp', 'java', 'node', 'python']
LABEL = {'rust': 'Rust', 'go': 'Go', 'csharp': 'C#/.NET', 'java': 'Java', 'node': 'TypeScript/Node', 'python': 'Python (now)'}
COLOR = {'rust': '#2a78d6', 'go': '#eb6834', 'csharp': '#1baf7a', 'java': '#eda100', 'node': '#e87ba4', 'python': '#898781'}
INK, MUTED, GRID = '#0b0b0b', '#52514e', '#e1e0d9'
plt.rcParams.update({'font.family': 'DejaVu Sans', 'font.size': 10, 'axes.edgecolor': '#c3c2b7', 'axes.labelcolor': MUTED, 'xtick.color': MUTED, 'ytick.color': MUTED,
                     'axes.spines.top': False, 'axes.spines.right': False, 'figure.facecolor': '#fcfcfb', 'axes.facecolor': '#fcfcfb', 'axes.titleweight': 'bold',
                     'axes.titlesize': 11, 'axes.titlelocation': 'left', 'savefig.dpi': 150})
N = 20000
L = [k for k in ORDER if k in R]


def save(fig, name):
    fig.tight_layout()
    fig.savefig(FIG / f'{name}.png')
    plt.close(fig)


def level(k, conc):
    return next(x for x in R[k]['http']['levels'] if x['conc'] == conc)


def bars(ax, values, fmt, log=False, horizontal=False):
    ks = L
    pos = range(len(ks))
    cols = [COLOR[k] for k in ks]
    vals = [values[k] for k in ks]
    if horizontal:
        ax.barh(pos, vals, color=cols, height=0.62)
        ax.set_yticks(list(pos)); ax.set_yticklabels([LABEL[k] for k in ks]); ax.invert_yaxis()
        if log: ax.set_xscale('log')
        for p, v in zip(pos, vals):
            ax.text(v, p, ' ' + fmt(v), va='center', ha='left', color=INK, fontsize=9)
        ax.set_xlim(right=max(vals) * (3 if log else 1.28)); ax.xaxis.grid(True, color=GRID); ax.set_axisbelow(True)
    else:
        ax.bar(pos, vals, color=cols, width=0.62)
        ax.set_xticks(list(pos)); ax.set_xticklabels([LABEL[k] for k in ks], rotation=20, ha='right')
        if log: ax.set_yscale('log')
        for p, v in zip(pos, vals):
            ax.text(p, v, fmt(v), ha='center', va='bottom', color=INK, fontsize=9)
        ax.set_ylim(top=max(vals) * (3 if log else 1.18)); ax.yaxis.grid(True, color=GRID); ax.set_axisbelow(True)


# ---- 1 raw rule-engine speed ---------------------------------------------------------------------------------------------
rate1 = {k: N / (R[k]['batch']['eval_1t_ms'] / 1000) for k in L}
ratem = {k: N / (R[k]['batch']['eval_mt_ms'] / 1000) for k in L}
fig, ax = plt.subplots(1, 2, figsize=(10, 3.6))
bars(ax[0], rate1, lambda v: f'{v/1000:,.0f}k', log=True, horizontal=True); ax[0].set_title('One core: claims evaluated per second')
bars(ax[1], ratem, lambda v: f'{v/1000:,.0f}k', log=True, horizontal=True); ax[1].set_title(f'All {R["_machine"]["cpus"]} cores: claims per second')
for a in ax: a.set_xlabel('claims per second (log scale, higher is better)')
save(fig, '01_engine_speed')

# ---- 2 parse versus evaluate (what a service really pays per claim) -----------------------------------------------------
fig, ax = plt.subplots(figsize=(8.5, 3.6))
pos = range(len(L))
parse = [R[k]['batch']['parse_ms'] * 1000 / N for k in L]
ev = [R[k]['batch']['eval_1t_ms'] * 1000 / N for k in L]
ax.barh(pos, parse, color=[COLOR[k] for k in L], height=0.6, label='reading the JSON (parse)')
ax.barh(pos, ev, left=parse, color=[COLOR[k] for k in L], height=0.6, alpha=0.45, hatch='///', edgecolor='#fcfcfb', label='applying the 15 rules (hatched)')
ax.set_yticks(list(pos)); ax.set_yticklabels([LABEL[k] for k in L]); ax.invert_yaxis()
for p, a, b in zip(pos, parse, ev):
    ax.text(a + b, p, f' {a + b:,.1f} us  (rules {100 * b / (a + b):.0f}%)', va='center', color=INK, fontsize=9)
ax.set_xlim(right=max(a + b for a, b in zip(parse, ev)) * 1.45); ax.set_xlabel('microseconds per claim, one core (lower is better)')
ax.set_title('Per claim, reading the JSON costs more than the rules in every language'); ax.xaxis.grid(True, color=GRID); ax.set_axisbelow(True)
ax.legend(loc='lower right', frameon=False, fontsize=8)
save(fig, '02_parse_vs_rules')

# ---- 4 / 5 HTTP throughput and latency ---------------------------------------------------------------------------------
levels = sorted({x['conc'] for k in L for x in R[k]['http']['levels']})
fig, ax = plt.subplots(figsize=(8, 3.8))
for k in L:
    ys = [level(k, c)['rps'] for c in levels]
    ax.plot(levels, ys, marker='o', color=COLOR[k], lw=2, ms=6, label=LABEL[k], mfc='#fcfcfb', mew=2)
    ax.text(levels[-1] * 1.07, ys[-1], LABEL[k], color=INK, va='center', fontsize=8)
ax.set_xscale('log', base=2); ax.set_xticks(levels); ax.set_xticklabels([str(c) for c in levels]); ax.set_xlim(right=levels[-1] * 3)
ax.set_xlabel('simultaneous connections'); ax.set_ylabel('requests per second'); ax.yaxis.grid(True, color=GRID); ax.set_axisbelow(True)
ax.set_title('HTTP service: requests per second (every answer verified)')
save(fig, '04_http_throughput')

fig, ax = plt.subplots(figsize=(8, 3.8))
for k in L:
    ys = [level(k, c)['p99_ms'] for c in levels]
    ax.plot(levels, ys, marker='o', color=COLOR[k], lw=2, ms=6, mfc='#fcfcfb', mew=2)
    ax.text(levels[-1] * 1.07, ys[-1], LABEL[k], color=INK, va='center', fontsize=8)
ax.set_xscale('log', base=2); ax.set_yscale('log'); ax.set_xticks(levels); ax.set_xticklabels([str(c) for c in levels]); ax.set_xlim(right=levels[-1] * 3)
ax.set_xlabel('simultaneous connections'); ax.set_ylabel('99th percentile latency, ms (log)'); ax.yaxis.grid(True, color=GRID); ax.set_axisbelow(True)
ax.set_title('HTTP service: the slowest 1% of requests')
save(fig, '05_http_p99')

# ---- 6 memory and efficiency ---------------------------------------------------------------------------------------------
fig, ax = plt.subplots(1, 3, figsize=(12, 3.6))
bars(ax[0], {k: level(k, 64)['server_rss_bytes'] / 2**20 for k in L}, lambda v: f'{v:,.0f}', horizontal=True); ax[0].set_title('Server memory under load (MB)')
bars(ax[1], {k: R[k]['batch']['peak_rss_bytes'] / 2**20 for k in L}, lambda v: f'{v:,.0f}', horizontal=True); ax[1].set_title('Batch of 20,000 claims: peak memory (MB)')
bars(ax[2], {k: level(k, 64)['rps'] / max(0.1, level(k, 64)['server_cores_used']) for k in L}, lambda v: f'{v/1000:.1f}k', horizontal=True); ax[2].set_title('Requests per second per core actually used')
save(fig, '06_memory_efficiency')

# ---- 7 build, size, start -----------------------------------------------------------------------------------------------
fig, ax = plt.subplots(1, 3, figsize=(12, 3.4))
bars(ax[0], {k: R[k]['build_s'] for k in L}, lambda v: f'{v:.0f} s' if v else 'none', horizontal=True); ax[0].set_title('Clean build time')
bars(ax[1], {k: R[k]['artifact_bytes'] / 2**20 for k in L}, lambda v: f'{v:.1f}', horizontal=True); ax[1].set_title('Application size (MB, without runtime)')
bars(ax[2], {k: R[k]['http']['cold_start_s'] for k in L}, lambda v: f'{v:.2f} s', horizontal=True); ax[2].set_title('Start until the first answer')
save(fig, '07_build_size_start')

# ---- 8 Amdahl: where the time really goes -------------------------------------------------------------------------------
AI_MEDIAN_S = 2.86
fig, ax = plt.subplots(figsize=(8.5, 3.4))
e2e = {k: (R[k]['batch']['parse_ms'] + R[k]['batch']['eval_1t_ms']) / N / 1000 for k in L}
vals = {k: AI_MEDIAN_S + e2e[k] for k in L}
bars(ax, vals, lambda v: f'{v:.4f} s', horizontal=True)
ax.set_xlim(0, AI_MEDIAN_S * 1.25); ax.set_xlabel('seconds per claim that needs an AI explanation (median model call = 2.86 s)')
ax.set_title('End to end the language changes the answer time by under 0.03%')
save(fig, '08_amdahl')

# ---- 9 robustness probe ----------------------------------------------------------------------------------------------------
cases = [c['case'] for c in R[L[0]]['http']['probe']['cases']]
fig, ax = plt.subplots(figsize=(9, 6.2))
cell = {}
for j, k in enumerate(L):
    for i, c in enumerate(R[k]['http']['probe']['cases']):
        st = c['status']
        ok = st in (200, 400, 413, 422)
        cell[(i, j)] = ok
        ax.scatter(j, i, s=230, marker='o' if ok else 'X', color='#006300' if ok else '#d03b3b', zorder=3)
        ax.text(j + 0.28, i, str(st if ok else ('500' if st == 500 else 'closed')), va='center', fontsize=7, color=MUTED)
ax.set_xticks(range(len(L))); ax.set_xticklabels([LABEL[k] for k in L], rotation=20, ha='right'); ax.set_yticks(range(len(cases))); ax.set_yticklabels(cases, fontsize=8); ax.invert_yaxis()
ax.set_xlim(-0.5, len(L) - 0.1); ax.grid(True, color=GRID); ax.set_axisbelow(True)
ax.set_title('22 hostile requests: handled (circle) or connection dropped (cross)')
save(fig, '09_robustness_probe')
clean = {k: sum(1 for c in R[k]['http']['probe']['cases'] if c['status'] in (200, 400, 413, 422)) for k in L}
n_cases = len(cases)

# ---- weighted decision -----------------------------------------------------------------------------------------------------
def lognorm(values, higher_better=True):
    lo, hi = min(values.values()), max(values.values())
    out = {}
    for k, v in values.items():
        if hi == lo:
            out[k] = 10.0
        else:
            t = (math.log(v) - math.log(lo)) / (math.log(hi) - math.log(lo))
            out[k] = 10 * (t if higher_better else 1 - t)
    return out


crit = {}
crit['Raw speed'] = lognorm({k: rate1[k] for k in L})
crit['Traffic handling'] = lognorm({k: (level(k, 64)['rps'] * level(k, 256)['rps']) ** 0.5 for k in L})
crit['Tail latency'] = lognorm({k: level(k, 256)['p99_ms'] for k in L}, higher_better=False)
crit['Memory and CPU efficiency'] = lognorm({k: level(k, 64)['rps'] / max(0.1, level(k, 64)['server_cores_used']) / max(1, level(k, 64)['server_rss_bytes'] / 2**20) ** 0.5 for k in L})
crit['All-cores batch throughput'] = lognorm({k: ratem[k] for k in L})
crit['Correct and clean under traffic'] = {k: 10.0 * (clean[k] / n_cases) * (1.0 if (level(k, 256)['errors'] == 0 and R[k]['mismatches'] == 0) else 0.7) for k in L}
crit['Bug resistance by design'] = {k: float(J['bug_resistance'][k]['score']) for k in L}
crit['Delivery fit for this project'] = {k: float(J['delivery_fit'][k]['score']) for k in L}
PROFILES = J['profiles']


def rank(weights):
    tot = {k: sum(weights[c] * crit[c][k] for c in weights) / sum(weights.values()) for k in L}
    return dict(sorted(tot.items(), key=lambda kv: -kv[1]))


fig, ax = plt.subplots(figsize=(8.5, 4))
base = rank(PROFILES['balanced'])
ks = list(base)
left = [0.0] * len(ks)
W = PROFILES['balanced']
shades = ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#4a3aa7', '#e34948', '#898781']
for i, c in enumerate(W):
    seg = [W[c] * crit[c][k] / sum(W.values()) for k in ks]
    ax.barh(range(len(ks)), seg, left=left, color=shades[i % len(shades)], height=0.6, edgecolor='#fcfcfb', label=c)
    left = [a + b for a, b in zip(left, seg)]
for i, k in enumerate(ks):
    ax.text(left[i], i, f' {left[i]:.1f}', va='center', color=INK, fontsize=9)
ax.set_yticks(range(len(ks))); ax.set_yticklabels([LABEL[k] for k in ks]); ax.invert_yaxis(); ax.set_xlim(0, 11); ax.set_xlabel('weighted score out of 10 (balanced weights)')
ax.legend(loc='upper center', bbox_to_anchor=(0.5, -0.18), ncol=3, frameon=False, fontsize=7.5)
ax.set_title('Weighted decision, balanced weights: contribution of each criterion')
save(fig, '10_weighted_score')

fig, ax = plt.subplots(figsize=(8.5, 3.6))
names = list(PROFILES)
width = 0.13
for j, k in enumerate(L):
    ax.bar([i + (j - len(L) / 2 + 0.5) * width for i in range(len(names))], [rank(PROFILES[n])[k] for n in names], width=width * 0.92, color=COLOR[k], label=LABEL[k])
ax.set_xticks(range(len(names))); ax.set_xticklabels([f'{n}\n{J["profile_notes"][n]}' for n in names], fontsize=8); ax.set_ylabel('weighted score out of 10')
ax.legend(ncol=6, frameon=False, fontsize=8, loc='upper center', bbox_to_anchor=(0.5, -0.28)); ax.yaxis.grid(True, color=GRID); ax.set_axisbelow(True)
ax.set_title('Does the winner depend on the weights? Four different priorities')
save(fig, '11_sensitivity')

# ---- tables for the document ---------------------------------------------------------------------------------------------
out = ['## Measured results (machine: %s CPUs, %s GB RAM, %s)\n' % (R['_machine']['cpus'], R['_machine']['ram_gb'], R['_machine']['os']), '',
       '| Language | Version | Parity (mismatches / 20,000) | Parse ms | Rules 1 core ms | Rules 16 cores ms | Batch peak MB | Build s | Cold start s |', '|---|---|---|---|---|---|---|---|---|']
for k in L:
    b = R[k]['batch']
    out.append(f"| {LABEL[k]} | {R[k]['version']} | {R[k]['mismatches']} | {b['parse_ms']:.0f} | {b['eval_1t_ms']:.0f} | {b['eval_mt_ms']:.0f} | {b['peak_rss_bytes']/2**20:.0f} | {R[k]['build_s']:.0f} | {R[k]['http']['cold_start_s']:.2f} |")
out += ['', '| Language | ' + ' | '.join(f'{c} conn: median req/s [min-max over 3 passes] (p99 ms)' for c in levels) + ' | Errors | Wrong answers | Server MB at 64 | Cores used at 64 |', '|---|' + '---|' * (len(levels) + 4)]
for k in L:
    ls = [level(k, c) for c in levels]
    out.append(f"| {LABEL[k]} | " + ' | '.join(f"{x['rps']:,.0f} [{x['rps_min']:,.0f}-{x['rps_max']:,.0f}] ({x['p99_ms']:.1f})" for x in ls) + f" | {sum(x['errors'] for x in ls)} | {sum(x['mismatches'] for x in ls)} | {level(k, 64)['server_rss_bytes']/2**20:.0f} | {level(k, 64)['server_cores_used']:.1f} |")
out += ['', f'| Language | Hostile requests handled cleanly (of {n_cases}) | Crashed or dropped |', '|---|---|---|']
for k in L:
    bad = [c['case'] for c in R[k]['http']['probe']['cases'] if c['status'] not in (200, 400, 413, 422)]
    out.append(f"| {LABEL[k]} | {clean[k]} | {', '.join(bad) or 'none'} |")
out += ['', '| Criterion | ' + ' | '.join(LABEL[k] for k in L) + ' |', '|---|' + '---|' * len(L)]
for c in crit:
    out.append(f'| {c} | ' + ' | '.join(f'{crit[c][k]:.1f}' for k in L) + ' |')
for n in PROFILES:
    r = rank(PROFILES[n])
    out.append(f'| **Total, {n}** | ' + ' | '.join(f'**{r[k]:.1f}**' for k in L) + ' |')
(HERE / 'results' / 'summary.md').write_text('\n'.join(out) + '\n', encoding='utf-8')
(HERE / 'results' / 'scores.json').write_text(json.dumps({'criteria': crit, 'totals': {n: rank(PROFILES[n]) for n in PROFILES}}, indent=1))
print('\n'.join(out))
