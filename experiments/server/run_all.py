#!/usr/bin/env python3
"""Run the server-side experiment suite (docs/65) in order, with the resource sampler beside it, and build the report.

    CG_URL=https://<server> CG_BADGE=CG-4001 CG_PASSWORD=... CG_TOTP_SEED=... \
    python experiments/server/run_all.py --run server_2026_11 [--only s01,s02] [--skip s05] [--docs <dir>] [--soak-minutes 30] \
        [--backup-key-file <file>] [--yes-restart]

Experiments (details in docs/65):  s01 model on the GPU · s02 claim pipeline and soak · s03 search by meaning · s04 documents · s07 integrity and backup ·
s05 restarts under load (only with --yes-restart) · s06 exposure scan (run it from another machine: python experiments/server/s06_exposure.py).
Every experiment writes outputs/server/<run>/<id>.json; the sampler writes samples.jsonl; run.json says what ran, how long it took and its exit code.
A failing experiment does not stop the others. The report (figures + docs/66 markdown) is built at the end when matplotlib is installed, else with
`python experiments/server/make_server_report.py --run <run>` on any machine that has it.
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import Run, host_facts, now_iso  # noqa: E402

HERE = Path(__file__).parent
ORDER = ['s01', 's02', 's03', 's04', 's07', 's05']
SCRIPT = {'s01': 's01_model.py', 's02': 's02_pipeline.py', 's03': 's03_search.py', 's04': 's04_documents.py', 's05': 's05_resilience.py', 's07': 's07_integrity.py'}


def command(exp, a):
    cmd = [sys.executable, str(HERE / SCRIPT[exp]), '--run', a.run] + (['--dry'] if a.dry else [])
    if exp == 's01':
        cmd += (['--base', a.model_base] if a.model_base else [])
    elif exp == 's02':
        cmd += ['--soak-minutes', str(a.soak_minutes), '--soak-rate', str(a.soak_rate)] + (['--stages', a.stages] if a.stages else [])
    elif exp == 's04':
        if not a.docs:
            return None
        cmd += ['--docs', a.docs]
    elif exp == 's05':
        if not a.yes_restart:
            return None
        cmd += ['--yes-restart', '--compose', a.compose]
    elif exp == 's07' and a.backup_key_file:
        cmd += ['--backup-key-file', a.backup_key_file, '--backup-dir', a.backup_dir]
    return cmd


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--only', default='')
    ap.add_argument('--skip', default='')
    ap.add_argument('--docs', default='', help='folder made by rust/tools/make_docs.py (enables s04)')
    ap.add_argument('--model-base', default='', help='OpenAI-compatible base URL of the model server if it is reachable from here')
    ap.add_argument('--stages', default='')
    ap.add_argument('--soak-minutes', type=float, default=0)
    ap.add_argument('--soak-rate', type=float, default=100)
    ap.add_argument('--backup-key-file', default='')
    ap.add_argument('--backup-dir', default='/tmp')
    ap.add_argument('--compose', default='docker compose -f deploy/rust/compose.yml --env-file .env')
    ap.add_argument('--yes-restart', action='store_true')
    ap.add_argument('--sample-every', type=float, default=5.0)
    ap.add_argument('--dry', action='store_true', help='mark the results as a dry run (a rehearsal on a laptop, not the server)')
    ap.add_argument('--no-report', action='store_true')
    a = ap.parse_args()
    run = Run(a.run, a.dry)
    wanted = [e for e in ORDER if (not a.only or e in a.only.split(',')) and e not in a.skip.split(',')]
    run.write('env', {'host': host_facts(), 'started': now_iso(), 'plan': wanted})
    stop = run.dir / 'sampler.stop'
    stop.unlink(missing_ok=True)
    sampler = subprocess.Popen([sys.executable, str(HERE / 'sampler.py'), '--out', str(run.dir / 'samples.jsonl'), '--every', str(a.sample_every), '--stop-file', str(stop)])
    log, t_all = [], time.time()
    try:
        for exp in wanted:
            cmd = command(exp, a)
            if cmd is None:
                log.append({'experiment': exp, 'skipped': 'needs ' + {'s04': '--docs <folder>', 's05': '--yes-restart'}[exp]})
                print(f'-- {exp}: skipped ({log[-1]["skipped"]})')
                continue
            print(f'== {exp}: {SCRIPT[exp]}')
            t0 = time.time()
            code = subprocess.call(cmd)
            log.append({'experiment': exp, 'exit_code': code, 'seconds': time.time() - t0, 'started_at': t0})
    finally:
        stop.write_text('stop')
        try:
            sampler.wait(timeout=30)
        except subprocess.TimeoutExpired:
            sampler.terminate()
    (run.dir / 'run.json').write_text(json.dumps({'run': a.run, 'dry_run': a.dry, 'started': t_all, 'seconds': time.time() - t_all, 'log': log}, indent=1), encoding='utf-8')
    if not a.no_report:
        subprocess.call([sys.executable, str(HERE / 'make_server_report.py'), '--run', a.run])
    failed = [x['experiment'] for x in log if x.get('exit_code')]
    print('done;', 'failed: ' + ', '.join(failed) if failed else 'every experiment ran')
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
