"""Footprint of the two servers, same machine, same demo data: start time, resident memory idle and after load, and request latency.

    python rust/tools/footprint.py [--requests 400]

Both servers run in --demo mode (60 sample claims, in-process workers). Latency is for one authenticated client sending requests one after
another over a fresh connection each (the worst case for a server) to GET /api/v1/claims?limit=50 and GET /api/v1/work/inbox; the numbers
are one laptop on one afternoon, to be read as an order of magnitude, not a benchmark.
"""
import argparse, json, os, statistics, subprocess, sys, time
from pathlib import Path

import psutil

sys.path.insert(0, str(Path(__file__).parent))
import conformance as c  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def rss_mb(proc):
    p = psutil.Process(proc.pid)
    total = p.memory_info().rss + sum(ch.memory_info().rss for ch in p.children(recursive=True))
    return round(total / 1048576, 1)


def measure(kind, port, n):
    data = Path(os.environ.get('TEMP', '/tmp')) / f'cg_fp_{kind}'
    data.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    proc = c.start(kind, port, data)
    started = time.time() - t0
    time.sleep(3)
    idle = rss_mb(proc)
    cl = c.Client(port, json.loads((data / 'demo_accounts.json').read_text()))
    cl.login('CG-2002')
    out = {}
    for name, path in (('claims_list', '/api/v1/claims?limit=50'), ('inbox', '/api/v1/work/inbox')):
        lat = []
        for _ in range(n):
            t = time.perf_counter()
            status, _ = cl.call('CG-2002', 'GET', path)
            lat.append((time.perf_counter() - t) * 1000)
            assert status == 200, (kind, path, status)
        lat.sort()
        out[name] = {'p50_ms': round(statistics.median(lat), 2), 'p95_ms': round(lat[int(len(lat) * 0.95)], 2), 'max_ms': round(lat[-1], 2)}
    loaded = rss_mb(proc)
    proc.kill()
    return {'start_seconds': round(started, 2), 'rss_idle_mb': idle, 'rss_after_load_mb': loaded, **out}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--requests', type=int, default=400)
    a = ap.parse_args()
    result = {'python': measure('py', 8463, a.requests), 'rust': measure('rs', 8464, a.requests)}
    print(json.dumps(result, indent=1))
    Path(ROOT / 'rust' / 'target' / 'footprint.json').write_text(json.dumps(result, indent=1))


if __name__ == '__main__':
    main()
