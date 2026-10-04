"""Fill the marked tables of docs/29_Test_Evaluation_Report.md from outputs/evaluation/{metrics,provenance}.json.

    python scripts/render_eval_report.py            # rewrite the tables in place
    python scripts/render_eval_report.py --check    # exit 1 if the committed report is stale

Prose in the report is written by hand; only the blocks between <!-- TABLE:name --> and <!-- /TABLE:name --> are generated,
so every number in a table traces to the JSON files and the JSON files trace to the provenance entries.
"""
import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPORT = ROOT / 'docs' / '29_Test_Evaluation_Report.md'
RULES = tuple(f'R{i:03d}' for i in range(1, 16))
TABLE_NAMES = ('provenance', 'tier_a', 'categories', 'per_rule', 'valid_claims', 'baselines', 'tier_b_c', 'latency', 'disagreements')


def fmt(x):
    if x is None:
        return 'n/a'
    if isinstance(x, bool):
        return str(x)
    if isinstance(x, int):
        return f'{x:,}'
    if isinstance(x, float):
        return f'{x:.4f}'
    return str(x).replace('|', '/')


def rate_cell(d):
    if not d or not d['n']:
        return 'n/a'
    return f"{d['k']:,}/{d['n']:,} (rate {fmt(d['rate'])}, 95% upper bound {fmt(d['upper95'])})"


def table(headers, rows):
    out = ['| ' + ' | '.join(headers) + ' |', '|' + '|'.join('---' for _ in headers) + '|']
    out.extend('| ' + ' | '.join(fmt(c) for c in row) + ' |' for row in rows)
    return '\n'.join(out)


def _sets(metrics, pred):
    items = [(sid, s) for sid, s in metrics['sets'].items() if pred(s['meta'])]
    return sorted(items, key=lambda kv: int(kv[0][1:]))


def render_tables(metrics, provenance):
    t = {}
    t['provenance'] = table(
        ['Set', 'Tier', 'Name', 'Claims scored', 'Results', 'Label source', 'Generator', 'Notes', 'File SHA-256 (first 8)', 'Limitation'],
        [[e['set_id'], e['tier'], e['name'], e['claims'], e['results'], e['label_source'], e['generator'],
          '; '.join(f'{k}={v}' for k, v in e['notes'].items()) or '-',
          ', '.join(f['sha256'][:8] for f in e['files']), e['limitation']]
         for e in sorted(provenance['sets'], key=lambda e: int(e['set_id'][1:]))])
    tier_a = _sets(metrics, lambda m: m['tier'] == 'A')
    t['tier_a'] = table(
        ['Set', 'Name', 'Claims', 'Results', 'Precision', 'Recall', 'F1 (FAIL)', 'Status accuracy', 'Disagreements',
         'Macro F1 by category', 'Categories undefined (no FAIL)', 'Macro F1 by severity', 'Macro F1 over rules'],
        [[sid, s['meta']['name'], s['summary']['claims'], s['summary']['results'], s['summary']['overall']['precision'],
          s['summary']['overall']['recall'], s['summary']['overall']['f1'], s['summary']['status_accuracy'],
          s['summary']['disagreements']['count'], s['summary']['macro_category']['macro_f1'],
          ', '.join(s['summary']['macro_category']['excluded']) or '-',
          s['summary'].get('macro_severity', {}).get('macro_f1'), s['summary']['macro_rule']['macro_f1']]
         for sid, s in tier_a])
    rows = []
    for sid, s in tier_a:
        if s['meta']['label_kind'] != 'organizer_key' or sid == 'S4':
            continue
        for cat, v in s['summary']['by_category'].items():
            rows.append([sid, cat, ', '.join(v['rules']), v['tp'], v['fp'], v['fn'], v['f1']])
    t['categories'] = table(['Set', 'Category', 'Rules', 'TP', 'FP', 'FN', 'F1 (pooled over the category)'], rows)
    per_sets = [(sid, s) for sid, s in tier_a if sid in ('S1', 'S2', 'S3')]
    rows = []
    for rid in RULES:
        row = [rid]
        for sid, s in per_sets:
            r = s['summary']['per_rule'].get(rid)
            row.append('n/a' if not r else f"{fmt(r['f1'])} ({r['tp'] + r['fn']} FAIL)")
        rows.append(row)
    t['per_rule'] = table(['Rule'] + [f'{sid} F1 (gold FAIL count)' for sid, _ in per_sets], rows)
    rows = []
    for sid, s in _sets(metrics, lambda m: True):
        v = s['summary']['valid_claims']
        if not any(x['n'] for x in v.values()):
            continue
        rows.append([sid, s['meta']['name'], rate_cell(v['claims_without_fail']), rate_cell(v['clean_claims']),
                     rate_cell(v['clean_claim_false_abstention']), rate_cell(v['clean_result_false_alarm'])])
    t['valid_claims'] = table(
        ['Set', 'Name', 'Claims with no gold FAIL: engine raised a FAIL', 'Fully clean claims: engine raised a FAIL',
         'Fully clean claims: engine abstained', 'Results on fully clean claims: false alarms'], rows)
    rows = []
    for sid, s in _sets(metrics, lambda m: True):
        for name, b in (s['baselines'] or {}).items():
            o = b['overall']
            rows.append([sid, name, o['precision'], o['recall'], o['f1'], b['status_accuracy']])
        if s['baselines']:
            o = s['summary']['overall']
            rows.append([sid, 'this system', o['precision'], o['recall'], o['f1'], s['summary']['status_accuracy']])
    t['baselines'] = table(['Set', 'Predictor', 'Precision', 'Recall', 'F1 (FAIL)', 'Status accuracy'], rows)
    rows = [[sid, s['meta']['name'], s['meta']['tier'], s['meta']['label_kind'], s['summary']['claims'],
             s['summary']['results'], s['summary']['disagreements']['count'], s['summary']['overall']['f1'],
             s['summary']['macro_category']['macro_f1'], s['engine_crashes']]
            for sid, s in _sets(metrics, lambda m: m['tier'] in ('B', 'C'))]
    t['tier_b_c'] = table(['Set', 'Name', 'Tier', 'Label kind', 'Claims', 'Results scored', 'Disagreements', 'F1 (FAIL)',
                           'Macro F1 by category', 'Engine crashes'], rows)
    rows = [[sid, d['rule_id'], d['gold'], d['predicted'], d['count']]
            for sid, s in _sets(metrics, lambda m: True) for d in s['summary']['disagreements'].get('breakdown', [])]
    t['disagreements'] = (table(['Set', 'Rule', 'Gold status', 'Engine status', 'Count'], rows)
                          if rows else 'No disagreements between the engine and the labels in any set.')
    lat = metrics['latency']
    names = (('engine_per_claim', 'Rule engine only'), ('ingest_fhir_per_claim', 'Ingest FHIR + engine'),
             ('ingest_csv_per_claim', 'Ingest CSV + engine'), ('audited_template_per_claim', 'Audited review, template explanation'))
    rows = [[label, lat[k]['n'], lat[k]['mean'], lat[k]['p50'], lat[k]['p95'], lat[k]['p99'], lat[k]['max']] for k, label in names]
    env = lat['environment']
    ai = metrics['ai_step_recorded']
    ai_line = (f"AI explanation step, recorded live calls: median {fmt(ai['seconds']['median'])} s, p95 {fmt(ai['seconds']['p95'])} s, "
               f"{ai['seconds']['calls']} calls ({ai['source']}).") if ai.get('seconds') else 'AI step latency: no recorded run found.'
    t['latency'] = (table(['Path', 'n', 'mean ms', 'p50 ms', 'p95 ms', 'p99 ms', 'max ms'], rows)
                    + f"\n\nMeasured on {env['platform']}, Python {env['python']}, {env['cpus']} CPUs; latency is machine-dependent "
                      f"and not deterministic.\n\n{ai_line}")
    return t


def apply_tables(doc, tables):
    for name, text in tables.items():
        pattern = re.compile(r'(<!-- TABLE:%s -->\n).*?(\n<!-- /TABLE:%s -->)' % (re.escape(name), re.escape(name)), re.DOTALL)
        if not pattern.search(doc):
            raise KeyError(name)
        doc = pattern.sub(lambda m: m.group(1) + text + m.group(2), doc, count=1)
    return doc


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--check', action='store_true')
    ap.add_argument('--evidence', default=str(ROOT / 'outputs' / 'evaluation'))
    a = ap.parse_args(argv)
    ev = Path(a.evidence)
    metrics = json.loads((ev / 'metrics.json').read_text(encoding='utf-8'))
    prov = json.loads((ev / 'provenance.json').read_text(encoding='utf-8'))
    doc = REPORT.read_text(encoding='utf-8')
    new = apply_tables(doc, render_tables(metrics, prov))
    if a.check:
        if new != doc:
            print('docs/29 is stale: run python scripts/render_eval_report.py', file=sys.stderr)
            return 1
        print('docs/29 matches the evidence')
        return 0
    with open(REPORT, 'w', encoding='utf-8', newline='\n') as f:
        f.write(new)
    print('updated', REPORT)
    return 0


if __name__ == '__main__':
    sys.exit(main())
