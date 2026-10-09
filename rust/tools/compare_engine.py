"""Differential test: the Rust engine against the Python engine, claim by claim.

    python rust/tools/compare_engine.py [--generated 3000] [--raw 3000] [--seed 20261009] [--cg rust/target/release/cg.exe]

The claims are the three public splits, generated claims that pass the transport contract, and raw generated claims (damaged and
random, many of which fail the contract and so exercise the fail-closed path). Both engines evaluate every claim; the results must be
identical in every field. Numbers are compared as numbers (the two sides may print 2.0 and 2 differently).

Exit code 0 only when every claim agrees. The Python engine is the reference: a disagreement is a bug in the Rust port until the
rulebook wording says otherwise.
"""
import argparse, json, random, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from engine_core import config, validate_transport  # noqa: E402
from yara_engine import evaluate, fail_closed_results  # noqa: E402
from claim_gen import random_claim  # noqa: E402


def python_results(claim, cfg):
    errors = []
    try:
        validate_transport(claim)
    except (ValueError, TypeError, KeyError, AttributeError):
        return fail_closed_results(claim, cfg), errors
    return evaluate(claim, cfg, errors), errors


def same(a, b, path=''):
    """Structural equality where 2 == 2.0 and key order does not matter. Returns a description of the first difference or None."""
    if isinstance(a, bool) or isinstance(b, bool):
        return None if a is b else f'{path}: {a!r} != {b!r}'
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return None if a == b else f'{path}: {a!r} != {b!r}'
    if isinstance(a, dict) and isinstance(b, dict):
        if set(a) != set(b):
            return f'{path}: keys {sorted(a)} != {sorted(b)}'
        for k in a:
            d = same(a[k], b[k], f'{path}/{k}')
            if d:
                return d
        return None
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return f'{path}: length {len(a)} != {len(b)}'
        for i, (x, y) in enumerate(zip(a, b)):
            d = same(x, y, f'{path}/{i}')
            if d:
                return d
        return None
    return None if a == b and type(a) is type(b) else f'{path}: {a!r} != {b!r}'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--generated', type=int, default=3000)
    ap.add_argument('--raw', type=int, default=3000)
    ap.add_argument('--seed', type=int, default=20261009)
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    ap.add_argument('--keep', default=str(ROOT / 'rust' / 'target' / 'golden'))
    a = ap.parse_args()
    out = Path(a.keep)
    out.mkdir(parents=True, exist_ok=True)
    cfg = config(ROOT)

    claims = []
    for split in ('development', 'validation', 'stress'):
        for line in (ROOT / 'data' / split / 'claims.jsonl').read_text(encoding='utf-8').splitlines():
            if line.strip():
                claims.append(('public:' + split, line))
    rng = random.Random(a.seed)
    sys.path.insert(0, str(ROOT / 'scripts'))
    from benchmark_yara_vs_python import generate_claims
    for c in generate_claims(a.generated, a.seed):
        claims.append(('generated', json.dumps(c, ensure_ascii=False)))
    for _ in range(a.raw):
        claims.append(('raw', json.dumps(random_claim(rng), ensure_ascii=False, allow_nan=False)))

    claims_file = out / 'claims.jsonl'
    claims_file.write_text('\n'.join(line for _, line in claims) + '\n', encoding='utf-8', newline='\n')
    t = time.perf_counter()
    expected = []
    for _, line in claims:
        c = json.loads(line)
        results, errors = python_results(c, cfg)
        expected.append({'claim_id': c.get('claim_id') if isinstance(c, dict) else None, 'results': results, 'tool_errors': errors})
    py_s = time.perf_counter() - t
    (out / 'python_results.jsonl').write_text('\n'.join(json.dumps(e) for e in expected) + '\n', encoding='utf-8', newline='\n')

    t = time.perf_counter()
    proc = subprocess.run([a.cg, 'evaluate', str(claims_file), '--root', str(ROOT)], capture_output=True, text=True, encoding='utf-8')
    rs_s = time.perf_counter() - t
    if proc.returncode != 0:
        print('cg failed:', proc.returncode, proc.stderr[-1500:])
        sys.exit(1)
    got = [json.loads(l) for l in proc.stdout.splitlines() if l.strip()]
    if len(got) != len(expected):
        print(f'cg produced {len(got)} records for {len(expected)} claims')
        sys.exit(1)

    bad = 0
    kinds = {}
    for (kind, line), e, g in zip(claims, expected, got):
        d = same(e, g)
        kinds.setdefault(kind, [0, 0])
        kinds[kind][0] += 1
        if d:
            bad += 1
            kinds[kind][1] += 1
            if bad <= 8:
                print(f'DIFF [{kind}] {e["claim_id"]}: {d}')
    print(f'{len(claims)} claims, python {py_s:.1f}s, rust {rs_s:.1f}s (including process start and rule-pack compile)')
    for k, (n, b) in kinds.items():
        print(f'  {k:20} {n:6} claims, {b} differ')
    print('ALL AGREE' if not bad else f'{bad} claims differ')
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
