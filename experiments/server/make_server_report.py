#!/usr/bin/env python3
"""Turn one run of the server experiments into figures and the results chapter (docs/65, docs/66).

    python experiments/server/make_server_report.py --run <name> [--out-doc docs/66_Server_Experiment_Results.md]

Reads outputs/server/<run>/*.json and samples.jsonl. Writes docs/figures/server/<run>_*.png and the markdown report (also kept in
outputs/server/<run>/report.md). Needs matplotlib (the same as rust/tools/make_charts.py). Every number in the text comes from the files; an experiment
that was not run says so, and is never filled with a guess. A run marked as a dry run carries a banner so a rehearsal is never read as the server's result.
"""
import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import OUT_ROOT, ROOT  # noqa: E402

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
except ImportError:
    plt = None

GREEN, GREY, RED, BLUE, AMBER = '#2a7f62', '#8da0b3', '#b3402f', '#2b6cb0', '#c98a1b'
GIB = 1024 ** 3


def load(run):
    d = OUT_ROOT / run
    if not d.is_dir():
        raise SystemExit(f'no results in {d}')
    data = {p.stem: json.loads(p.read_text(encoding='utf-8')) for p in d.glob('*.json')}
    samples = []
    sp = d / 'samples.jsonl'
    if sp.exists():
        for line in sp.read_text(encoding='utf-8').splitlines():
            try:
                samples.append(json.loads(line))
            except ValueError:
                pass
    return d, data, samples


class Figures:
    def __init__(self, run, enabled, directory=None, link='figures/server'):
        self.run, self.enabled, self.dir, self.link = run, enabled and plt is not None, Path(directory) if directory else ROOT / 'docs' / 'figures' / 'server', link
        self.dir.mkdir(parents=True, exist_ok=True)
        self.made = []

    def save(self, fig, name, alt):
        if not self.enabled:
            return ''
        fn = f'{self.run}_{name}.png'
        fig.tight_layout()
        fig.savefig(self.dir / fn, dpi=150)
        plt.close(fig)
        self.made.append(fn)
        return f'![{alt}]({self.link}/{fn})\n'


def fmt(x, nd=1, unit=''):
    return '-' if x is None else f'{x:.{nd}f}{unit}'


def pf(ok):
    return 'not measured' if ok is None else ('PASS' if ok else 'FAIL')


# ---- sampler-derived facts ------------------------------------------------------------------------------------------------------------
def resource_facts(samples, log):
    f = {'n': len(samples)}
    if not samples:
        return f
    t0 = samples[0]['t']
    f['duration_s'] = samples[-1]['t'] - t0
    peaks, limits, totals = {}, {}, []
    for s in samples:
        tot = 0
        for c in s.get('containers', []):
            peaks[c['name']] = max(peaks.get(c['name'], 0), c['mem_bytes'])
            limits[c['name']] = c['mem_limit_bytes']
            tot += c['mem_bytes']
        totals.append((s['t'] - t0, tot))
    f.update(peaks=peaks, limits=limits, totals=totals, peak_total=max((t for _, t in totals), default=0))
    hosts = [s['host'] for s in samples if s.get('host', {}).get('mem_total')]
    f['host_total'] = hosts[0]['mem_total'] if hosts else None
    f['host_min_available'] = min((h['mem_available'] for h in hosts if h.get('mem_available')), default=None)
    gpus = [s['gpu'] for s in samples if s.get('gpu')]
    if gpus:
        f['gpu'] = {'mem_peak_mib': max(g['mem_used_mib'] for g in gpus), 'mem_total_mib': gpus[0]['mem_total_mib'], 'util_peak': max(g['util_pct'] for g in gpus),
                    'temp_peak': max(g['temp_c'] for g in gpus), 'power_peak': max(g['power_w'] for g in gpus), 'series': [(s['t'] - t0, s['gpu']) for s in samples if s.get('gpu')]}
    spans = {e['experiment']: (e['started_at'] - t0, e['started_at'] - t0 + e['seconds']) for e in log if 'started_at' in e}
    f['spans'] = spans
    if 's02' in spans:
        a, b = spans['s02']
        pts = [(t, v) for t, v in totals if a <= t <= b]
        if len(pts) >= 8:  # least-squares slope of total container memory over the pipeline experiment (and soak), in MiB per hour
            n, mt, mv = len(pts), sum(t for t, _ in pts) / len(pts), sum(v for _, v in pts) / len(pts)
            den = sum((t - mt) ** 2 for t, _ in pts)
            f['growth_mib_per_hour'] = (sum((t - mt) * (v - mv) for t, v in pts) / den) * 3600 / 1024 ** 2 if den else 0.0
    return f


def shade(ax, spans, names):
    cols = {'s01': '#dbe7f3', 's02': '#e1f0e8', 's03': '#f6ecd5', 's04': '#efe0f0', 's05': '#f5dcd7', 's07': '#e8e8e8'}
    for k, (a, b) in spans.items():
        ax.axvspan(a / 60, b / 60, color=cols.get(k, '#eee'), alpha=0.8, label=names.get(k, k))


NAMES = {'s01': 'S1 model', 's02': 'S2 pipeline', 's03': 'S3 search', 's04': 'S4 documents', 's05': 'S5 restarts', 's07': 'S7 integrity'}


def make_figures(fig, data, rf):
    out = {}
    s1 = data.get('s01_model', {}).get('result')
    if s1:
        lv = s1['levels']
        f, ax = plt.subplots(figsize=(7, 3.8)) if fig.enabled else (None, None)
        if f:
            ax.plot([r['concurrency'] for r in lv], [r['requests_per_minute'] for r in lv], marker='o', color=GREEN, label='explanations per minute')
            ax.axhline(200, color=RED, ls='--', lw=1, label='design target (200 a minute)')
            ax.set_xscale('log', base=2); ax.set_xlabel('concurrent requests'); ax.set_ylabel('explanations per minute'); ax.grid(alpha=.3)
            ax2 = ax.twinx(); ax2.plot([r['concurrency'] for r in lv], [r['latency_p95_s'] for r in lv], marker='s', color=GREY, label='latency, 95th percentile')
            ax2.set_ylabel('seconds'); h1, l1 = ax.get_legend_handles_labels(); h2, l2 = ax2.get_legend_handles_labels(); ax.legend(h1 + h2, l1 + l2, loc='lower right', fontsize=8)
            ax.set_title('S1. The model on the GPU')
            out['s1'] = fig.save(f, 'model', 'Explanations per minute and latency against concurrency')
    s2 = data.get('s02_pipeline')
    if s2 and s2.get('stages'):
        st = s2['stages']
        f, axs = plt.subplots(1, 2, figsize=(9, 3.6)) if fig.enabled else (None, None)
        if f:
            lab = [f"{s['batch_size']}" for s in st]
            axs[0].bar(lab, [s['claims_per_minute'] for s in st], color=GREEN); axs[0].axhline(200, color=RED, ls='--', lw=1)
            axs[0].set_xlabel('claims in a batch'); axs[0].set_ylabel('claims per minute'); axs[0].set_title('Throughput by batch size'); axs[0].grid(axis='y', alpha=.3)
            axs[1].bar(lab, [(s['per_claim_ms'] or {}).get('p50') or 0 for s in st], color=BLUE, label='median'); axs[1].plot(lab, [(s['per_claim_ms'] or {}).get('p95') or 0 for s in st], 'o-', color=AMBER, label='95th percentile')
            axs[1].set_xlabel('claims in a batch'); axs[1].set_ylabel('milliseconds per claim'); axs[1].set_title('Time per claim'); axs[1].legend(fontsize=8); axs[1].grid(axis='y', alpha=.3)
            out['s2'] = fig.save(f, 'pipeline', 'Throughput and time per claim by batch size')
    if s2 and s2.get('soak'):
        sk = s2['soak']
        f, ax = plt.subplots(figsize=(7.5, 3.6)) if fig.enabled else (None, None)
        if f:
            ax.plot([m['minute'] for m in sk], [m['claims'] for m in sk], color=GREEN, label='claims taken in per minute'); ax.set_xlabel('minute of the soak'); ax.set_ylabel('claims per minute'); ax.grid(alpha=.3)
            ax2 = ax.twinx(); ax2.plot([m['minute'] for m in sk], [m['batch_done_p95_s'] or 0 for m in sk], color=GREY, label='batch time, 95th percentile (s)'); ax2.set_ylabel('seconds')
            h1, l1 = ax.get_legend_handles_labels(); h2, l2 = ax2.get_legend_handles_labels(); ax.legend(h1 + h2, l1 + l2, fontsize=8, loc='lower right'); ax.set_title('S2. Steady load (soak)')
            out['soak'] = fig.save(f, 'soak', 'Claims per minute and batch time during the soak')
    if rf.get('gpu') and fig.enabled:
        g = rf['gpu']['series']
        f, ax = plt.subplots(figsize=(9, 3.8))
        shade(ax, rf['spans'], NAMES)
        ax.plot([t / 60 for t, _ in g], [x['mem_used_mib'] / 1024 for _, x in g], color=GREEN, label='GPU memory used (GiB)')
        ax.axhline(rf['gpu']['mem_total_mib'] / 1024, color=RED, ls='--', lw=1, label='card memory')
        ax.set_xlabel('minutes since the start'); ax.set_ylabel('GiB'); ax.grid(alpha=.3)
        ax2 = ax.twinx(); ax2.plot([t / 60 for t, _ in g], [x['util_pct'] for _, x in g], color=BLUE, alpha=.6, lw=1, label='GPU utilisation (%)'); ax2.set_ylim(0, 105); ax2.set_ylabel('%')
        h1, l1 = ax.get_legend_handles_labels(); h2, l2 = ax2.get_legend_handles_labels(); ax.legend(h1 + h2, l1 + l2, fontsize=7, loc='upper left', ncol=2); ax.set_title('S0. The GPU during the experiments')
        out['gpu'] = fig.save(f, 'gpu', 'GPU memory and utilisation over time')
    if rf.get('peaks') and fig.enabled:
        names = sorted(rf['peaks'], key=lambda n: rf['peaks'][n])
        f, ax = plt.subplots(figsize=(8, 0.45 * len(names) + 1.6))
        ax.barh(names, [rf['peaks'][n] / GIB for n in names], color=GREEN, label='peak used')
        for i, n in enumerate(names):
            lim = rf['limits'].get(n, 0)
            if lim and lim < 64 * GIB:
                ax.plot([lim / GIB, lim / GIB], [i - .4, i + .4], color=RED, lw=2)
        ax.set_xlabel('GiB (red mark: the container memory limit)'); ax.set_title('S0. Peak memory by container'); ax.grid(axis='x', alpha=.3)
        out['mem'] = fig.save(f, 'memory_peaks', 'Peak memory by container against its limit')
        f, ax = plt.subplots(figsize=(9, 3.8))
        shade(ax, rf['spans'], NAMES)
        ax.plot([t / 60 for t, _ in rf['totals']], [v / GIB for _, v in rf['totals']], color=GREEN, label='all containers together')
        if rf.get('host_total'):
            ax.axhline(rf['host_total'] / GIB, color=RED, ls='--', lw=1, label='server memory')
        ax.set_xlabel('minutes since the start'); ax.set_ylabel('GiB'); ax.legend(fontsize=7, ncol=2, loc='upper left'); ax.grid(alpha=.3); ax.set_title('S0. Memory of the whole stack over time')
        out['memtime'] = fig.save(f, 'memory_timeline', 'Memory of all containers over time')
    s3 = data.get('s03_search')
    if s3 and fig.enabled:
        sp = s3['speed']
        f, axs = plt.subplots(1, 2, figsize=(9, 3.6))
        q = s3['quality']
        axs[0].bar(['questions\nanswered', 'off-topic\nrefused'], [q['hit_rate'] * 100, q['refusal_rate'] * 100], color=[GREEN, BLUE]); axs[0].set_ylim(0, 105); axs[0].set_ylabel('%')
        axs[0].set_title('Quality'); axs[0].grid(axis='y', alpha=.3)
        axs[1].plot([r['concurrency'] for r in sp], [(r['latency_s'] or {}).get('p50') or 0 for r in sp], 'o-', color=GREEN, label='median'); axs[1].plot([r['concurrency'] for r in sp], [(r['latency_s'] or {}).get('p95') or 0 for r in sp], 's-', color=AMBER, label='95th percentile')
        axs[1].set_xlabel('concurrent questions'); axs[1].set_ylabel('seconds'); axs[1].set_title('Latency'); axs[1].legend(fontsize=8); axs[1].grid(alpha=.3)
        out['s3'] = fig.save(f, 'search', 'Search quality and latency')
    s4 = data.get('s04_documents')
    if s4 and s4.get('conditions') and fig.enabled:
        cs = s4['conditions']
        f, axs = plt.subplots(1, 2, figsize=(9, 3.6))
        axs[0].bar([c['condition'] for c in cs], [(c['field_accuracy'] or 0) * 100 for c in cs], color=GREEN); axs[0].set_ylim(0, 105); axs[0].set_ylabel('% of fields read exactly'); axs[0].set_title('Accuracy'); axs[0].tick_params(axis='x', labelsize=8)
        axs[1].bar([c['condition'] for c in cs], [(c['seconds_per_document'] or {}).get('p50') or 0 for c in cs], color=BLUE); axs[1].set_ylabel('seconds per document (median)'); axs[1].set_title('Speed'); axs[1].tick_params(axis='x', labelsize=8)
        for ax in axs:
            ax.grid(axis='y', alpha=.3)
        out['s4'] = fig.save(f, 'documents', 'Document reading accuracy and speed')
    s5 = data.get('s05_resilience')
    if s5 and s5.get('services') and fig.enabled:
        sv = s5['services']
        f, ax = plt.subplots(figsize=(7, 3.4))
        ax.bar([r['service'] for r in sv], [r['unhealthy_s'] for r in sv], color=[GREEN if r['acknowledged_but_missing'] == 0 else RED for r in sv])
        ax.set_ylabel('seconds the server answered "unhealthy"'); ax.set_title('S5. Restart of each service under load (red: a claim was lost)'); ax.grid(axis='y', alpha=.3)
        out['s5'] = fig.save(f, 's5_restarts', 'Seconds of unavailability per restarted service')
    return out


# ---- the markdown ---------------------------------------------------------------------------------------------------------------------
def scoreboard(data, rf):
    rows = []

    def add(exp, what, target, got, ok):
        rows.append((exp, what, target, got, pf(ok)))
    s1 = data.get('s01_model')
    if s1 and s1.get('ok'):
        r = s1['result']
        add('S1', 'Explanations per minute (best level)', '>= 200', fmt(r['best_requests_per_minute']) + f" at {r['at_concurrency']} concurrent", r['reaches_target'])
        add('S1', 'Model requests that failed', '0', str(sum(x['errors'] for x in r['levels'])), s1['checks']['no_errors'])
    s2 = data.get('s02_pipeline')
    if s2:
        c = s2['checks']
        add('S2', 'Claims per minute through the whole pipeline (best stage)', '>= 200', fmt(c['best_claims_per_minute']), c['reaches_200_per_minute'])
        add('S2', 'Batches that failed', '0', str(sum(x['error_count'] for x in s2['stages']) + sum(m['errors'] for m in s2['soak'])), c['no_errors'])
        if s2.get('responsive_after'):
            worst = max((v.get('p95') or 0) for v in s2['responsive_after'].values())
            add('S2', 'Slowest read request right after the load (95th percentile)', '< 2 s', fmt(worst, 2, ' s'), c.get('still_responsive_after_load'))
    s3 = data.get('s03_search')
    if s3:
        q = s3['quality']
        add('S3', 'Rulebook questions answered with the right rule', '>= 90 %', fmt(q['hit_rate'] * 100, 0, ' %'), s3['checks']['hit_rate_at_least_0_9'])
        add('S3', 'Off-topic strings refused', 'all', f"{q['refused']} of {q['off_topic']}", s3['checks']['all_off_topic_refused'])
    s4 = data.get('s04_documents')
    if s4:
        add('S4', 'Fields read exactly from documents', '>= 95 %', fmt((s4['field_accuracy'] or 0) * 100, 1, ' %') if s4['field_accuracy'] is not None else '-', s4['checks']['field_accuracy_at_least_0_95'])
        add('S4', 'Documents that could not be read', '0', str(len(s4['failed'])), s4['checks']['no_failed_documents'])
    s5 = data.get('s05_resilience')
    if s5:
        add('S5', 'Acknowledged claims lost in restarts', '0', str(sum(r['acknowledged_but_missing'] for r in s5['services'])), s5['checks']['nothing_acknowledged_was_lost'])
        add('S5', 'Services back after restart', 'all', f"{sum(r['recovered'] for r in s5['services'])} of {len(s5['services'])}", s5['checks']['every_service_recovered'])
    s6 = data.get('s06_exposure')
    if s6:
        for k, v in s6['checks'].items():
            add('S6', k.replace('_', ' '), 'yes', 'yes' if v else 'no', v)
    s7 = data.get('s07_integrity')
    if s7:
        for k, v in s7['checks'].items():
            add('S7', k.replace('_', ' '), 'yes', 'yes' if v else 'no', v)
    if rf.get('peak_total') and rf.get('host_total'):
        add('S0', 'Peak memory of all containers vs server memory', '<= 85 %', fmt(rf['peak_total'] / rf['host_total'] * 100, 0, ' %') + f" of {rf['host_total'] / GIB:.1f} GiB", rf['peak_total'] <= 0.85 * rf['host_total'])
    if rf.get('gpu'):
        g = rf['gpu']
        add('S0', 'Peak GPU memory vs the card', '<= 95 %', fmt(g['mem_peak_mib'] / g['mem_total_mib'] * 100, 0, ' %') + f" of {g['mem_total_mib'] / 1024:.0f} GiB", g['mem_peak_mib'] <= 0.95 * g['mem_total_mib'])
    over = [n for n, v in rf.get('peaks', {}).items() if rf['limits'].get(n) and rf['limits'][n] < 64 * GIB and v > 0.95 * rf['limits'][n]]
    if rf.get('peaks'):
        add('S0', 'Containers that reached their memory limit', 'none', ', '.join(over) or 'none', not over)
    if rf.get('growth_mib_per_hour') is not None:
        add('S0', 'Memory growth during the pipeline experiment', '< 100 MiB/hour', fmt(rf['growth_mib_per_hour'], 0, ' MiB/h'), rf['growth_mib_per_hour'] < 100)
    return rows


def table(head, rows):
    return '| ' + ' | '.join(head) + ' |\n|' + '|'.join('---' for _ in head) + '|\n' + '\n'.join('| ' + ' | '.join(str(c) for c in r) + ' |' for r in rows) + '\n'


def renumber(text):
    """The sections that were written depend on what ran; number them 1, 2, 3 ... in order of appearance."""
    n, out = 0, []
    for line in text.split('\n'):
        m = re.match(r'^## \d+\. (.*)$', line)
        if m:
            n += 1
            line = f'## {n}. {m.group(1)}'
        out.append(line)
    return '\n'.join(out)


def build(run, data, samples, figs, rf, runinfo):
    env = (data.get('env') or {}).get('host', {})
    dry = any(v.get('dry_run') for v in data.values() if isinstance(v, dict))
    L = [f'# 66. Server experiment results: run `{run}`\n']
    if dry:
        L.append('> **DRY RUN.** These files come from a rehearsal of the scripts on a development machine, not from the target server. The numbers show that the pipeline works, not how the server performs. Do not quote them as results.\n')
    L.append('Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/%s/`.\n' % run)
    L.append('## 1. The machine\n')
    L.append(table(['Item', 'Value'], [[k, v if v is not None else 'not available'] for k, v in [
        ('Host', env.get('hostname')), ('Operating system', env.get('platform')), ('CPU', f"{env.get('cpu_model')} ({env.get('cpu_count')} threads)"),
        ('Memory', f"{env['mem_total_bytes'] / GIB:.1f} GiB" if env.get('mem_total_bytes') else None), ('GPU', env.get('gpu')), ('Docker', env.get('docker')),
        ('Compose', env.get('compose')), ('Code version', env.get('git_commit')), ('Run started', (data.get('env') or {}).get('started'))]]))
    L.append('## 2. Scoreboard\n')
    sb = scoreboard(data, rf)
    L.append(table(['Experiment', 'Measure', 'Target', 'Result', 'Verdict'], sb) if sb else 'Nothing was measured in this run.\n')
    L.append(f"{sum(1 for r in sb if r[4] == 'PASS')} of {len(sb)} measures pass; {sum(1 for r in sb if r[4] == 'FAIL')} fail.\n")
    ran = {e['experiment'] for e in (runinfo or {}).get('log', []) if 'exit_code' in e}
    not_run = [f'{e["experiment"]} ({e["skipped"]})' for e in (runinfo or {}).get('log', []) if 'skipped' in e]
    if 's06_exposure' not in data:
        not_run.append('s06 (run it from another machine)')
    if not_run:
        L.append('Not run in this session: ' + '; '.join(not_run) + '.\n')
    s1 = data.get('s01_model')
    if s1 and s1.get('ok'):
        r = s1['result']
        L.append(f"## 3. S1. The explanation model on the GPU\n\nModel `{s1['model']}`, one explanation per request (about 250 tokens in, up to 160 out, temperature 0).\n")
        L.append(figs.get('s1', ''))
        L.append(table(['Concurrent', 'Requests', 'Errors', 'Per minute', 'Latency p50 (s)', 'Latency p95 (s)', 'Tokens/s'], [[x['concurrency'], x['requests'], x['errors'], x['requests_per_minute'], x['latency_p50_s'], x['latency_p95_s'], x['output_tokens_per_second']] for x in r['levels']]))
        L.append(f"Best: **{r['best_requests_per_minute']} a minute** at {r['at_concurrency']} concurrent requests; GPU memory peak during the benchmark: {str(r['gpu_memory_peak_mib']) + ' MiB' if r.get('gpu_memory_peak_mib') else 'not measured (no GPU reading)'}.\n")
    if s2 := data.get('s02_pipeline'):
        L.append('## 4. S2. The claim pipeline\n\nClaims go through the proxy, the API, the queue, the engine and the store (POST /api/v1/queue/submit, asynchronous), in batches of rising size.\n')
        L.append(figs.get('s2', ''))
        L.append(table(['Batch', 'Batches', 'Claims in', 'Errors', 'Claims/min', 'Accept p50 (s)', 'Batch done p50 (s)', 'Batch done p95 (s)', 'ms per claim p50'], [[s['batch_size'], s['batches'], s['claims_ok'], s['error_count'], fmt(s['claims_per_minute']), fmt(s['accept_s'].get('p50'), 2), fmt(s['done_s'].get('p50'), 2), fmt(s['done_s'].get('p95'), 2), fmt((s['per_claim_ms'] or {}).get('p50'), 0)] for s in s2['stages']]))
        if s2.get('responsive_after'):
            L.append('Answer time of three read requests (seconds), idle before the load and right after it:\n\n' + table(['Request', 'Before p50', 'Before p95', 'After p50', 'After p95'], [[k, fmt(s2['responsive_before'].get(k, {}).get('p50'), 3), fmt(s2['responsive_before'].get(k, {}).get('p95'), 3), fmt(v.get('p50'), 3), fmt(v.get('p95'), 3) + (f" ({v['not_ok']} refused)" if v.get('not_ok') else '')] for k, v in s2['responsive_after'].items()]))
        if s2.get('soak'):
            L.append('Steady load (soak):\n\n' + figs.get('soak', '') + f"\n{len(s2['soak'])} minute(s), {sum(m['claims'] for m in s2['soak'])} claims, {sum(m['errors'] for m in s2['soak'])} failed batches.\n")
    if rf.get('n'):
        L.append(f"## 5. S0. Resources during the run\n\n{rf['n']} samples over {rf['duration_s'] / 60:.0f} minutes (container memory and CPU from `docker stats`, GPU from `nvidia-smi`).\n")
        for k in ('gpu', 'mem', 'memtime'):
            L.append(figs.get(k, ''))
        if rf.get('peaks'):
            L.append(table(['Container', 'Peak memory (MiB)', 'Limit (MiB)', 'Share of limit'], [[n, f"{v / 1024 ** 2:.0f}", f"{rf['limits'][n] / 1024 ** 2:.0f}" if rf['limits'][n] < 64 * GIB else 'none', fmt(v / rf['limits'][n] * 100, 0, ' %') if rf['limits'][n] < 64 * GIB else '-'] for n, v in sorted(rf['peaks'].items(), key=lambda kv: -kv[1])]))
        if rf.get('gpu'):
            g = rf['gpu']
            L.append(f"GPU peaks: memory {g['mem_peak_mib']:.0f} of {g['mem_total_mib']:.0f} MiB, utilisation {g['util_peak']:.0f} %, temperature {g['temp_peak']:.0f} C, power {g['power_peak']:.0f} W.\n")
        if rf.get('host_min_available') and rf.get('host_total'):
            L.append(f"The server never had less than {rf['host_min_available'] / GIB:.1f} GiB of {rf['host_total'] / GIB:.1f} GiB available.\n")
        if rf.get('growth_mib_per_hour') is not None:
            L.append(f"Memory trend of the whole stack during S2: {rf['growth_mib_per_hour']:+.0f} MiB per hour (a straight-line fit; a steady climb over a long soak means a leak, a flat line does not).\n")
    if s3 := data.get('s03_search'):
        q = s3['quality']
        L.append('## 6. S3. Ask the rulebook, by meaning\n\n' + figs.get('s3', '') + f"\n{q['hits']} of {q['answerable']} questions returned the right rule ({fmt(q['hit_rate'] * 100, 0, ' %')}); {q['refused']} of {q['off_topic']} off-topic strings were refused. Rate-limited answers (429): {s3['rate_limited_429']}. HTTP statuses of the quality pass: {json.dumps(s3.get('statuses_first_pass'))}.\n")
        L.append(table(['Concurrent', 'Questions', 'Answered', 'Per second', 'Latency p50 (s)', 'Latency p95 (s)'], [[x['concurrency'], x['requests'], x['ok'], fmt(x['questions_per_second'], 1), fmt(x['latency_s'].get('p50'), 3), fmt(x['latency_s'].get('p95'), 3)] for x in s3['speed']]))
        if q['misses']:
            L.append('Questions that missed: ' + '; '.join(f"\"{m['q']}\" (wanted {', '.join(m['want'])})" for m in q['misses'][:8]) + '.\n')
    if s4 := data.get('s04_documents'):
        L.append('## 7. S4. Documents to claims\n\n' + figs.get('s4', '') + f"\n{s4['files_tried']} documents tried, {len(s4['failed'])} failed, {s4['busy_retries']} waits because the reader was busy.\n" + ('Failures: ' + '; '.join(f"{x['file']} (HTTP {x['status']}: {x['detail']})" for x in s4['failed'][:5]) + '.\n' if s4['failed'] else ''))
        L.append(table(['Condition', 'Documents', 'Field accuracy', 'Documents fully right', 'Seconds p50', 'Seconds p95'], [[c['condition'], c['documents'], fmt((c['field_accuracy'] or 0) * 100, 1, ' %'), fmt((c['documents_all_fields_right'] or 0) * 100, 0, ' %'), fmt(c['seconds_per_document'].get('p50'), 1), fmt(c['seconds_per_document'].get('p95'), 1)] for c in s4['conditions']]))
        L.append(f"For comparison: {s4['reference']}.\n")
    if s5 := data.get('s05_resilience'):
        L.append('## 8. S5. Restarts under load\n\n' + figs.get('s5', '') + '\n' + table(['Service', 'Restart command (s)', 'Unhealthy (s)', 'Recovered', 'Sent', 'Acknowledged', 'Failed during restart', 'Acknowledged but missing'], [[r['service'], fmt(r['restart_command_s'], 1), fmt(r['unhealthy_s'], 1), 'yes' if r['recovered'] else 'NO', r['sent'], r['acknowledged'], r['failed_during_restart'], r['acknowledged_but_missing']] for r in s5['services']]))
    if s6 := data.get('s06_exposure'):
        L.append('## 9. S6. Exposure seen from the network\n\n' + table(['Port', 'Service', 'Open'], [[p, v['service'], 'yes' if v['open'] else 'no'] for p, v in sorted(s6['ports'].items(), key=lambda kv: int(kv[0]))]))
        t = s6['tls']
        tls_text = f"TLS could not be reached on port 443 ({t.get('error', 'no answer')})." if not t.get('negotiated') else f"TLS: older protocols {json.dumps(t.get('old_protocols'))}, negotiated {t.get('negotiated')}, certificate {t.get('subject', '?')} issued by {t.get('issuer', '?')}, {t.get('days_left', '?')} days left."
        L.append(f"{tls_text} Headers: {json.dumps(s6['headers'])}.\n")
    if s7 := data.get('s07_integrity'):
        L.append(f"## 10. S7. Integrity and backup\n\nHealth panel: {s7['health']['overall']} ({fmt(s7['health']['seconds'], 2)} s). Audit verification: HTTP {s7['audit_verify']['status']}. Ledger: HTTP {s7['ledger']['status']}.\n")
        if s7['health'].get('probes'):
            L.append(table(['Health probe', 'Status'], [[p['name'], p['status']] for p in s7['health']['probes']]))
        if b := s7.get('backup'):
            L.append(f"Backup: {fmt(b['seconds'], 1)} s, {fmt((b.get('bytes') or 0) / 1024 ** 2, 1)} MiB, encrypted: {b.get('looks_encrypted')}, checksum file matches: {b.get('sha256_matches')}.\n")
    L.append('## 11. How to read this\n\nA FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.\n')
    return renumber('\n'.join(x for x in L if x))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--out-doc', default='', help='default: docs/66_Server_Experiment_Results.md; a dry run writes only into its own run folder')
    ap.add_argument('--no-figures', action='store_true')
    a = ap.parse_args()
    d, data, samples = load(a.run)
    if plt is None and not a.no_figures:
        print('matplotlib is not installed: writing the report without figures (pip install matplotlib, then run this again)')
    runinfo = data.get('run')
    rf = resource_facts(samples, (runinfo or {}).get('log', []))
    dry = any(v.get('dry_run') for v in data.values() if isinstance(v, dict))
    if dry and not a.out_doc:       # a rehearsal must never overwrite the results chapter or put figures among the real ones
        figs = Figures(a.run, not a.no_figures, d / 'figures', 'figures')
        out_doc = d / 'report.md'
    else:
        figs = Figures(a.run, not a.no_figures)
        out_doc = Path(a.out_doc or ROOT / 'docs' / '66_Server_Experiment_Results.md')
    out = make_figures(figs, data, rf) if figs.enabled else {}
    text = build(a.run, data, samples, out, rf, runinfo)
    out_doc.write_text(text, encoding='utf-8')
    if out_doc != d / 'report.md':
        (d / 'report.md').write_text(text.replace('](figures/server/', '](../../../docs/figures/server/'), encoding='utf-8')
    print(f'wrote {out_doc} and {len(figs.made)} figures in {figs.dir}')


if __name__ == '__main__':
    main()
