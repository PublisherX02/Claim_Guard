"""Phase 2 detection evaluation: score every labelled set, measure latency, write outputs/evaluation/*.json.

    python scripts/evaluate_phase2.py --out-dir outputs/evaluation

Accuracy results are deterministic (fixed seeds). Latency results depend on the machine and are labelled as such.
"""
import argparse
import json
import logging
import os
import platform
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
import eval_sets as sets  # noqa: E402
from audit_log import AuditLog, audited_review  # noqa: E402
from engine_core import baseline, config  # noqa: E402
from eval_metrics import Tally, latency_summary, severity_groups  # noqa: E402
from ingest import CSV_FOLDER, FHIR, ingest  # noqa: E402
from llm_adapter import MockExplanationProvider  # noqa: E402
from yara_engine import evaluate  # noqa: E402


def score_set(es, cfg, severity, baselines=False):
    tally, always_pass, starter = Tally(), Tally(), Tally()
    crashes, seconds = 0, []
    for claim, gold in es.items():
        errors = []
        t0 = time.perf_counter()
        results = evaluate(claim, cfg, errors)
        seconds.append(time.perf_counter() - t0)
        pred = {r['rule_id']: r['status'] for r in results}
        crashes += bool(errors)
        tally.add_claim(claim['claim_id'], gold, pred)
        if baselines:
            always_pass.add_claim(claim['claim_id'], gold, {r: 'PASS' for r in gold})
            starter.add_claim(claim['claim_id'], gold, {r['rule_id']: r['status'] for r in baseline(claim, cfg)})
    return {
        'summary': tally.summary(severity), 'engine_crashes': crashes, 'engine_seconds': seconds,
        'baselines': {'always_pass': always_pass.summary(), 'starter_baseline': starter.summary()} if baselines else None,
    }


def _ms(values):
    return latency_summary([v * 1000 for v in values])


def measure_latency(root, cfg, repeats=3, audited_claims=100):
    root = Path(root)
    claims = [c for es in sets.organizer_sets(root)[:3] for c, _ in es.items()]
    engine = []
    for _ in range(repeats):
        for c in claims:
            t0 = time.perf_counter()
            evaluate(c, cfg, [])
            engine.append(time.perf_counter() - t0)
    per_format = {}
    for key, fmt, name in (('ingest_fhir_per_claim', FHIR, 'fhir_bundles.jsonl'), ('ingest_csv_per_claim', CSV_FOLDER, 'csv')):
        times = []
        for _ in range(repeats):
            it = iter(ingest(root / 'data' / 'development' / name, fmt))
            while True:
                t0 = time.perf_counter()
                item = next(it, None)
                if item is None:
                    break
                if item.accepted:
                    evaluate(item.claim, cfg, [])
                times.append(time.perf_counter() - t0)
        per_format[key] = _ms(times)
    audited = []
    with tempfile.TemporaryDirectory() as tmp:
        log = AuditLog(Path(tmp) / 'audit.jsonl')
        provider = MockExplanationProvider()
        for c in claims[:audited_claims]:
            t0 = time.perf_counter()
            audited_review(log, c, cfg, provider=provider, fallback=provider)
            audited.append(time.perf_counter() - t0)
    return {
        'unit': 'milliseconds', 'deterministic': False,
        'environment': {'platform': platform.platform(), 'python': platform.python_version(), 'cpus': os.cpu_count()},
        'engine_per_claim': _ms(engine), **per_format, 'audited_template_per_claim': _ms(audited),
    }


def recorded_ai_step(root):
    """Median, p95 and call count of the recorded live AI explanation calls, or None if that record is absent or malformed."""
    try:
        value = json.loads((Path(root) / 'outputs' / 'defense' / 'load.json').read_text(encoding='utf-8'))['ai_step_seconds']
    except (OSError, ValueError, KeyError, TypeError):
        return None
    return value if isinstance(value, dict) else None


def run_all(root, out_dir, generated=107635, mutant_attempts=37000, repeats=3, audited_claims=100):
    root, out_dir = Path(root), Path(out_dir)
    cfg = config(root)
    severity = severity_groups(cfg['rules'])
    commit = sets.read_git_head(root)
    all_sets = [*sets.organizer_sets(root), *sets.format_variant_sets(root), sets.generated_set(root, generated),
                sets.mutant_set(root, mutant_attempts), sets.boundary_set(root)]
    all_sets.sort(key=lambda s: int(s.set_id[1:]))
    logging.disable(logging.WARNING)           # the engine logs each isolated rule crash; it is counted instead
    metrics, provenance = {}, []
    try:
        for es in all_sets:
            r = score_set(es, cfg, severity, baselines=(es.tier == 'A' and es.path == 'engine'))
            metrics[es.set_id] = {'meta': {'tier': es.tier, 'name': es.name, 'path': es.path, 'label_kind': es.label_kind},
                                  'summary': r['summary'], 'engine_crashes': r['engine_crashes'], 'baselines': r['baselines'],
                                  'engine_ms': _ms(r['engine_seconds'])}
            provenance.append(sets.provenance_entry(es, root, commit, r['summary']['claims'], r['summary']['results']))
        latency = measure_latency(root, cfg, repeats, audited_claims)
    finally:
        logging.disable(logging.NOTSET)
    ai = recorded_ai_step(root)
    out = {'commit': commit, 'sets': metrics, 'latency': latency,
           'ai_step_recorded': {'seconds': ai, 'source': 'outputs/defense/load.json (live Mistral-Nemo calls, recorded earlier; not re-run here)'}}
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / 'metrics.json').write_text(json.dumps(out, indent=2), encoding='utf-8')
    (out_dir / 'provenance.json').write_text(json.dumps({'commit': commit, 'sets': provenance}, indent=2), encoding='utf-8')
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out-dir', default='outputs/evaluation')
    ap.add_argument('--generated', type=int, default=107635)
    ap.add_argument('--mutants', type=int, default=37000)
    a = ap.parse_args(argv)
    m = run_all(ROOT, a.out_dir, a.generated, a.mutants)
    for sid, s in m['sets'].items():
        o = s['summary']['overall']
        print(sid, s['meta']['name'], f"claims={s['summary']['claims']}", f"f1={o['f1']}", f"disagreements={s['summary']['disagreements']['count']}")
    return 0


if __name__ == '__main__':
    sys.exit(main())
