#!/usr/bin/env python3
"""Resource sampler: runs beside the experiments and writes one JSON line every few seconds (docs/65, experiment S0).

    python experiments/server/sampler.py --out outputs/server/<run>/samples.jsonl [--every 5] [--stop-file <path>]

Each line: {"t": unix seconds, "gpu": {mem_used_mib, mem_total_mib, util_pct, temp_c, power_w} or null,
            "containers": [{name, mem_bytes, mem_limit_bytes, cpu_pct}], "host": {mem_total, mem_available}}.
It stops when the stop file appears or on Ctrl-C/SIGTERM. Missing tools (no nvidia-smi, no docker) give null/empty, never an error.
"""
import argparse
import json
import shutil
import signal
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import parse_size, sh  # noqa: E402

STOP = False


def _stop(*_):
    global STOP
    STOP = True


def gpu():
    if not shutil.which('nvidia-smi'):
        return None
    code, out = sh('nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu,temperature.gpu,power.draw --format=csv,noheader,nounits', timeout=15)
    if code != 0 or not out:
        return None
    try:
        a = [x.strip() for x in out.splitlines()[0].split(',')]
        return {'mem_used_mib': float(a[0]), 'mem_total_mib': float(a[1]), 'util_pct': float(a[2]), 'temp_c': float(a[3]), 'power_w': float(a[4])}
    except (ValueError, IndexError):
        return None


def containers():
    if not shutil.which('docker'):
        return []
    code, out = sh(['docker', 'stats', '--no-stream', '--format', '{{.Name}}|{{.MemUsage}}|{{.CPUPerc}}'], timeout=30)
    rows = []
    for line in out.splitlines() if code == 0 else []:
        try:
            name, mem, cpu = line.split('|')
            used, limit = mem.split('/')
            rows.append({'name': name, 'mem_bytes': parse_size(used), 'mem_limit_bytes': parse_size(limit), 'cpu_pct': float(cpu.strip().rstrip('%'))})
        except ValueError:
            continue
    return rows


def host():
    try:
        kv = {}
        for line in Path('/proc/meminfo').read_text().splitlines():
            k, v = line.split(':', 1)
            kv[k] = int(v.split()[0]) * 1024
        return {'mem_total': kv.get('MemTotal'), 'mem_available': kv.get('MemAvailable')}
    except (OSError, ValueError):
        return {'mem_total': None, 'mem_available': None}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', required=True)
    ap.add_argument('--every', type=float, default=5.0)
    ap.add_argument('--stop-file', default='')
    a = ap.parse_args()
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    stop_file = Path(a.stop_file) if a.stop_file else None
    with out.open('a', encoding='utf-8') as f:
        while not STOP and not (stop_file and stop_file.exists()):
            t0 = time.time()
            f.write(json.dumps({'t': t0, 'gpu': gpu(), 'containers': containers(), 'host': host()}) + '\n')
            f.flush()
            time.sleep(max(0.0, a.every - (time.time() - t0)))


if __name__ == '__main__':
    main()
