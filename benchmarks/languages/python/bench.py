"""Control: the current Python implementation (tests/oracle.py, the independent 15-rule reference) on the same corpus.

    python bench.py -corpus ../corpus [-threads N] [-repeat R] | [-serve host:port]

Multi-core here means processes (one interpreter per core), because the GIL stops threads running Python in parallel.
"""
import argparse, json, multiprocessing as mp, platform, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tests'))
import oracle  # noqa: E402

CODE = {oracle.PASS: 'P', oracle.FAIL: 'F', oracle.UNABLE: 'U', oracle.NA: 'N'}


def evaluate_line(c, pack):
    r = oracle.evaluate(c, pack)
    return ''.join(CODE[r[k]] for k in sorted(r))


def worker(corpus, lines, conn):
    pack = load_pack(corpus)
    claims = [json.loads(l) for l in lines]
    conn.send('ready')
    while conn.recv() == 'eval':
        t = time.perf_counter()
        for c in claims:
            oracle.evaluate(c, pack)
        conn.send((time.perf_counter() - t) * 1000)


def load_pack(corpus):
    return {n: json.loads((Path(corpus) / 'pack' / f'{n}.json').read_text(encoding='utf-8')) for n in ('policies', 'services')}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('-corpus', required=True); ap.add_argument('-threads', type=int, default=mp.cpu_count())
    ap.add_argument('-repeat', type=int, default=5); ap.add_argument('-serve', default='')
    a = ap.parse_args()
    if a.serve:
        sys.exit('use serve_python.py for the HTTP mode')
    pack = load_pack(a.corpus)
    t = time.perf_counter()
    lines = [l for l in Path(a.corpus, 'claims.jsonl').read_text(encoding='utf-8').split('\n') if l]
    read_ms = (time.perf_counter() - t) * 1000
    t = time.perf_counter()
    claims = [json.loads(l) for l in lines]
    parse_ms = (time.perf_counter() - t) * 1000
    expected = Path(a.corpus, 'expected.txt').read_text().split('\n')
    mismatches = sum(1 for c, e in zip(claims, expected) if evaluate_line(c, pack) != e)
    one = []
    for _ in range(a.repeat):
        t = time.perf_counter()
        for c in claims:
            oracle.evaluate(c, pack)
        one.append((time.perf_counter() - t) * 1000)
    chunk = -(-len(lines) // a.threads)
    procs = []
    for k in range(a.threads):
        parent, child = mp.Pipe()
        p = mp.Process(target=worker, args=(a.corpus, lines[k * chunk:(k + 1) * chunk], child), daemon=True)
        p.start(); procs.append((p, parent))
    for _, c in procs:
        c.recv()
    many = []
    for _ in range(a.repeat):
        t = time.perf_counter()
        for _, c in procs:
            c.send('eval')
        for _, c in procs:
            c.recv()
        many.append((time.perf_counter() - t) * 1000)
    for _, c in procs:
        c.send('stop')
    med = lambda v: sorted(v)[len(v) // 2]
    print(json.dumps({'lang': 'python', 'version': platform.python_version(), 'claims': len(claims), 'mismatches': mismatches, 'threads': a.threads,
                      'read_ms': read_ms, 'parse_ms': parse_ms, 'eval_1t_ms': med(one), 'eval_mt_ms': med(many)}))


if __name__ == '__main__':
    mp.freeze_support()
    main()
