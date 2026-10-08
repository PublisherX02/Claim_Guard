"""Experiment: does a second, format-only pass improve the queue's AI explanations? (docs/32, section "Two-pass AI")

    python scripts/two_pass_experiment.py --provider featherless --claims 40 --out outputs/experiments/two_pass.json
    python scripts/two_pass_experiment.py --dry-run          # plumbing check with a scripted fake model; proves nothing about quality

PRE-REGISTERED DECISION RULE (written before any live run, so the result cannot choose its own yardstick):
  Unit: one flagged finding from data/development/claims.jsonl. Both arms see the same findings in the same order.
  Arm A: one pass (TemplateModel two_pass=False). Arm B: two passes (two_pass=True).
  Primary metric: share of findings whose template, once filled, is accepted by the grounding guard (workqueue.explain.default_guard).
  Secondary: model-call failure rate, p50 and p95 latency per finding, calls per finding.
  ADOPT two-pass only if B's guard acceptance is at least 5 percentage points above A's AND B's failure rate is not higher than A's
  by more than 2 points AND B's p95 latency is at most 2.2 times A's. Otherwise keep one pass. With fewer than 30 findings per arm the
  result is reported as inconclusive whatever the numbers are.
  Each finding is asked once per arm at temperature 0; no cherry-picking, every call is written to the output file.
Nothing here has been run against a live model as of this commit: there was no model key or GPU in the session that wrote it.
"""
import argparse
import json
import statistics
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from engine_core import config, load_jsonl
from workqueue import breaker as brk
from workqueue.explain import default_guard, failure_shape, fill
from workqueue.model_adapter import PROMPT_VERSION, TemplateModel, from_provider
from yara_engine import evaluate

MIN_PER_ARM = 30
CEILING = 90.0


def findings(n):
    cfg = config(ROOT)
    out = []
    for claim in load_jsonl(ROOT / 'data' / 'development' / 'claims.jsonl'):
        for r in evaluate(claim, cfg):
            if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
                out.append(r)
        if len(out) >= n:
            break
    return out[:n], {r['rule_id']: r for r in cfg['rules']}


def run_arm(model, results):
    rows = []
    for r in results:
        request = {'rule_id': r['rule_id'], 'failure_shape': failure_shape(r), 'prompt_version': PROMPT_VERSION,
                   'placeholders': ['{value}', '{line}']}
        t0, calls0 = time.monotonic(), model.calls
        row = {'claim_id': r['claim_id'], 'rule_id': r['rule_id']}
        try:
            template = model(request, time.monotonic() + CEILING)
            row.update(ok=True, template=template, guard_accepted=bool(default_guard(fill(template, r), r)))
        except (brk.Transient, brk.Fatal) as exc:
            row.update(ok=False, error=f'{type(exc).__name__}: {exc}', guard_accepted=False)
        row.update(seconds=round(time.monotonic() - t0, 3), calls=model.calls - calls0)
        rows.append(row)
    return rows


def summarise(rows):
    secs = sorted(x['seconds'] for x in rows)
    return {'n': len(rows), 'guard_accepted': sum(x['guard_accepted'] for x in rows) / len(rows),
            'failure_rate': sum(not x['ok'] for x in rows) / len(rows), 'p50_seconds': statistics.median(secs),
            'p95_seconds': secs[min(len(secs) - 1, int(0.95 * len(secs)))], 'calls_per_finding': sum(x['calls'] for x in rows) / len(rows)}


def decide(a, b):
    if min(a['n'], b['n']) < MIN_PER_ARM:
        return 'inconclusive (fewer than %d findings per arm)' % MIN_PER_ARM
    better = b['guard_accepted'] - a['guard_accepted'] >= 0.05
    safe = b['failure_rate'] - a['failure_rate'] <= 0.02
    fast = b['p95_seconds'] <= 2.2 * max(a['p95_seconds'], 1e-9)
    return 'adopt two-pass' if (better and safe and fast) else 'keep one pass'


def fake_complete(prompt, timeout):
    if prompt.startswith('Put the text below'):
        text = prompt.split('## Text\n', 1)[1].strip()
        return json.dumps({'template': text})
    return 'Rule failed because {value} on line {line} does not meet the rule. Request the missing source information from the provider.'


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--provider', choices=['featherless', 'ollama', 'nvidia'])
    p.add_argument('--claims', type=int, default=40, help='number of flagged findings per arm')
    p.add_argument('--out', default=str(ROOT / 'outputs' / 'experiments' / 'two_pass.json'))
    p.add_argument('--dry-run', action='store_true', help='use a scripted fake model; checks the plumbing only')
    a = p.parse_args(argv)
    if not a.dry_run and not a.provider:
        p.error('give --provider, or --dry-run')
    results, rules = findings(a.claims)
    if a.dry_run:
        models = {'one_pass': TemplateModel(fake_complete, rules, name='fake'), 'two_pass': TemplateModel(fake_complete, rules, name='fake', two_pass=True)}
    else:
        import llm_adapter
        cls = {'featherless': llm_adapter.FeatherlessExplanationProvider, 'ollama': llm_adapter.OllamaExplanationProvider,
               'nvidia': llm_adapter.NvidiaExplanationProvider}[a.provider]
        provider = cls()
        models = {'one_pass': from_provider(provider, rules), 'two_pass': from_provider(provider, rules, two_pass=True)}
    rows = {arm: run_arm(m, results) for arm, m in models.items()}
    summary = {arm: summarise(r) for arm, r in rows.items()}
    verdict = decide(summary['one_pass'], summary['two_pass'])
    out = {'dry_run': a.dry_run, 'provider': a.provider, 'prompt_version': PROMPT_VERSION, 'summary': summary, 'decision': verdict,
           'note': 'a dry run uses a scripted fake model and says nothing about quality' if a.dry_run else None, 'rows': rows}
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    Path(a.out).write_text(json.dumps(out, indent=2, ensure_ascii=False), encoding='utf-8')
    print(json.dumps({'summary': summary, 'decision': verdict}, indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())
