"""Phase 2 detection evaluation of the RUST engine, scored with the same code as the Python report (src/eval_metrics.py).

    python rust/tools/phase2_eval.py [--out outputs/evaluation/rust_metrics.json]

For each public split (data/stress = the 50-claim split, data/validation = 150, data/development = 400) it runs `cg evaluate`, compares every
result with the organizers' answer key (expected_results.jsonl), and reports: FAIL detection precision/recall/F1 overall, per rule and per rule
category, the macro F1 over categories and over rules, status accuracy, the false-positive rate on claims the key says are valid (with an exact
95% upper bound), and the engine's time per claim (the difference between a large and a small run, so process start-up cancels out).
It also checks that the severity / human-review flags are set as the output schema demands for every FAIL and UNABLE_TO_ASSESS result.
"""
import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
from eval_metrics import Tally, severity_groups  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
SPLITS = (('stress', '50-claim split (data/stress)'), ('validation', 'data/validation (150 claims)'), ('development', 'data/development (400 claims)'))


def load(path):
    return [json.loads(l) for l in Path(path).read_text(encoding='utf-8').splitlines() if l.strip()]


def run_engine(claims_path):
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT))
    t0 = time.perf_counter()
    p = subprocess.run([CG, 'evaluate', str(claims_path)], capture_output=True, text=True, env=env, encoding='utf-8')
    dt = time.perf_counter() - t0
    if p.returncode != 0:
        raise SystemExit('cg evaluate failed: ' + p.stderr[-300:])
    return [json.loads(l) for l in p.stdout.splitlines() if l.strip()], dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', default=str(ROOT / 'outputs' / 'evaluation' / 'rust_metrics.json'))
    a = ap.parse_args()
    rules = json.loads((ROOT / 'rules' / 'rules.json').read_text(encoding='utf-8'))
    severity = severity_groups(rules) if rules else None
    report, times = {}, {}
    for split, label in SPLITS:
        d = ROOT / 'data' / split
        claims = load(d / 'claims.jsonl')
        gold_rows = load(d / 'expected_results.jsonl')
        gold = {}
        for r in gold_rows:
            gold.setdefault(r['claim_id'], {})[r['rule_id']] = r['status']
        out, dt = run_engine(d / 'claims.jsonl')
        times[split] = (len(claims), dt)
        pred = {row['claim_id']: {r['rule_id']: r['status'] for r in row['results']} for row in out}
        flags_ok = all(r['requires_human_review'] and r['corrective_action'] for row in out for r in row['results'] if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'))
        tally = Tally()
        for c in claims:
            tally.add_claim(c['claim_id'], gold[c['claim_id']], pred[c['claim_id']])
        s = tally.summary(severity)
        s['label'] = label
        s['flags_set_for_every_fail_and_abstention'] = flags_ok
        report[split] = s
    (n_small, t_small), (n_big, t_big) = times['stress'], times['development']
    per_claim_ms = max(0.0, (t_big - t_small) / (n_big - n_small) * 1000)
    report['latency'] = {'runs': {k: {'claims': v[0], 'seconds': round(v[1], 3)} for k, v in times.items()},
                         'engine_ms_per_claim_by_difference': round(per_claim_ms, 3),
                         'note': 'whole-process wall time of `cg evaluate` (includes loading the YARA rules); the per-claim figure is the slope between the 50 and 400 claim runs'}
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    Path(a.out).write_text(json.dumps(report, indent=1), encoding='utf-8')
    for split, label in SPLITS:
        s = report[split]
        ov, vc = s['overall'], s['valid_claims']
        print('%-12s claims %3d  FAIL F1 %.4f  P %.4f  R %.4f  macro(category) %.4f  macro(rule, defined only) %.4f  status acc %.4f  disagreements %d  valid claims flagged %d/%d (95%% upper %.3f)  flags ok %s' % (
            split, s['claims'], ov['f1'] or 0, ov['precision'] or 0, ov['recall'] or 0, s['macro_category'].get('macro_f1') or 0, s['macro_rule'].get('macro_f1') or 0,
            s['status_accuracy'], s['disagreements']['count'], vc['claims_without_fail']['k'], vc['claims_without_fail']['n'], vc['claims_without_fail']['upper95'] or 0,
            s['flags_set_for_every_fail_and_abstention']))
    print('engine time per claim by difference: %.3f ms (50 claims %.2fs, 400 claims %.2fs whole-process)' % (per_claim_ms, t_small, t_big))
    print('wrote', a.out)


if __name__ == '__main__':
    main()
