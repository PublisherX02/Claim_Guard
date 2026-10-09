"""Build the shared benchmark corpus: claims.jsonl (generated, transport-valid), expected.txt (oracle statuses), pack/.

    python benchmarks/languages/gen_corpus.py [--claims 20000] [--seed 20260927]

Expected output per claim is one line of 15 status codes (P pass, F fail, U unable, N not applicable) from tests/oracle.py,
the independent reference that was already checked against the answer key. Every language port must reproduce it exactly.
"""
import argparse, json, shutil, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src')); sys.path.insert(0, str(ROOT / 'tests')); sys.path.insert(0, str(ROOT / 'scripts'))
import oracle  # noqa: E402
from benchmark_yara_vs_python import generate_claims  # noqa: E402

CODE = {oracle.PASS: 'P', oracle.FAIL: 'F', oracle.UNABLE: 'U', oracle.NA: 'N'}


def main():
    ap = argparse.ArgumentParser(); ap.add_argument('--claims', type=int, default=20000); ap.add_argument('--seed', type=int, default=20260927)
    a = ap.parse_args()
    out = Path(__file__).resolve().parent / 'corpus'; (out / 'pack').mkdir(parents=True, exist_ok=True)
    for n in ('policies', 'services'):
        shutil.copy(ROOT / 'rules' / f'{n}.json', out / 'pack' / f'{n}.json')
    pack = oracle.load_rules_pack(ROOT)
    claims = generate_claims(a.claims, a.seed)
    with open(out / 'claims.jsonl', 'w', encoding='utf-8', newline='\n') as f, open(out / 'expected.txt', 'w', newline='\n') as e:
        for c in claims:
            f.write(json.dumps(c, ensure_ascii=False, allow_nan=False) + '\n')
            r = oracle.evaluate(c, pack)
            e.write(''.join(CODE[r[k]] for k in sorted(r)) + '\n')
    print(f'{len(claims)} claims -> {out}')


if __name__ == '__main__':
    main()
