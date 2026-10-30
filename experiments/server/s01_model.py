#!/usr/bin/env python3
"""S1. The explanation model (vLLM) on the GPU: requests per minute, latency and GPU memory at rising concurrency (docs/65).

    python experiments/server/s01_model.py --run <name> [--base http://vllm:8000/v1] [--model google/gemma-3-4b-it] [--levels 1,2,4,8,16,32,64]

Without --base it runs rust/tools/model_bench.py inside a throw-away python container on the compose network, because the model server is not
published on the host (that is the point of the deployment). VLLM_API_KEY is read from the environment or from .env. The question it answers:
does one RTX 3080 reach the 200 explanations a minute that the design assumes, and how much of the card does it take?
"""
import argparse
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import ROOT, Run, sh  # noqa: E402


def env_value(name):
    if os.environ.get(name):
        return os.environ[name]
    p = ROOT / '.env'
    if p.exists():
        for line in p.read_text(encoding='utf-8').splitlines():
            if line.startswith(name + '='):
                return line.split('=', 1)[1].strip()
    return ''


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--base', default='')
    ap.add_argument('--model', default='')
    ap.add_argument('--levels', default='1,2,4,8,16,32,64')
    ap.add_argument('--per-level', type=int, default=4)
    ap.add_argument('--network', default='', help='docker network of the compose project (default: found from the vllm container)')
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run = Run(a.run, a.dry)
    model, key = a.model or env_value('VLLM_MODEL') or 'google/gemma-3-4b-it', env_value('VLLM_API_KEY')
    name = 'server_' + model.split('/')[-1].replace('.', '_')
    if a.base:
        cmd = [sys.executable, str(ROOT / 'rust' / 'tools' / 'model_bench.py'), '--base', a.base, '--model', model, '--name', name, '--levels', a.levels, '--per-level', str(a.per_level)]
        if key:
            cmd += ['--api-key', key]
    else:
        code, net = sh("docker inspect $(docker ps -q --filter name=vllm | head -1) --format '{{range $k,$v := .NetworkSettings.Networks}}{{$k}} {{end}}'")
        net = a.network or (net.split()[0] if code == 0 and net else '')
        if not net:
            raise SystemExit('no running vllm container found; start the stack with the ai option or pass --base')
        cmd = ['docker', 'run', '--rm', '--network', net, '-v', f'{ROOT}:/repo', 'python:3.12-slim', 'python', '/repo/rust/tools/model_bench.py', '--base', 'http://vllm:8000/v1',
               '--model', model, '--name', name, '--levels', a.levels, '--per-level', str(a.per_level)] + (['--api-key', key] if key else [])
    print('S1: running the model benchmark (several minutes)')
    code, out = sh(cmd, timeout=3 * 3600)
    src = ROOT / 'outputs' / 'defense' / f'model_bench_{name}.json'
    if code != 0 or not src.exists():
        run.write('s01_model', {'ok': False, 'error': out[-600:]})
        raise SystemExit('model benchmark failed:\n' + out[-600:])
    rep = json.loads(src.read_text(encoding='utf-8'))
    run.write('s01_model', {'ok': True, 'model': model, 'result': rep,
                            'checks': {'reaches_200_per_minute': rep['reaches_target'], 'no_errors': all(r['errors'] == 0 for r in rep['levels'])}})


if __name__ == '__main__':
    main()
