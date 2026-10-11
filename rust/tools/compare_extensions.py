"""Differential test of the extension (advisory) rules: Rust against Python, claim by claim.

    python rust/tools/compare_extensions.py [--scenarios 4000] [--raw 2000] [--seed 20261010]

Scenario claims are built to make every rule fire, pass, be not-applicable and be unable to assess (pairs on the same or different
dates, modifiers, pharmaceutical lines with and without a route, accident and secondary-only diagnoses with and without event dates,
repeated lines, shared authorizations, daily limits, damaged quantities and dates); raw claims are the random/damaged generators.
Earlier claims come from the same file for both sides. Every field of every result must agree.
"""
import argparse, copy, json, random, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
sys.path.insert(0, str(Path(__file__).parent))
from compare_engine import same  # noqa: E402
from claim_gen import random_claim, damaged_coherent_claim  # noqa: E402
from claim_history import InMemoryHistory  # noqa: E402
from extension_rules import evaluate_extensions  # noqa: E402
import ext_world as w  # noqa: E402

CODES = [w.PRIMARY, w.COMPONENT, w.NEVER, w.RX, 'SVC-EDU-01', 'SVC-EDU-02', 'SVC-EDU-03', 'SVC-NOPE']
LIMITED = list(json.loads((ROOT / 'rules' / 'services.json').read_text()).keys())[:6]
DATES = ['2026-03-01', '2026-03-02', '2026-03-02', '2026-03-03', '2026-02-30', 'not-a-date', None, '2026-03-02 ']
MODS = [None, None, w.SEPARATE, 'EDU-ROUTE-ORAL', 'EDU-ROUTE-IV', 'BAD']
QTY = [1, 1, 2, 3, 5, 0, -1, 2.5, 1.0, None, True, 'x', 10 ** 20]
DX = ['DX-EDU-01', 'DX-EXT-ACCIDENT', 'DX-EXT-SECONDARY', 'DX-NOPE', None, '']
NOTES = [None, 'Synthetic claim.', 'Event date: 2026-02-28', 'Event date:2026-03-02', 'Event date: 2026-03-05', 'Event date: 2026-03-0', 'Event date: 2026-03-011',
         'x Event date: 2026-02-31 y Event date: 2026-01-01', 5]


def scenario(rng, n):
    lines = []
    for k in range(rng.randint(0, 5)):
        row = {'line_id': rng.choice([f'L{k}', f'L{k}', '', None, 7]), 'service_code': rng.choice(CODES + LIMITED), 'service_date': rng.choice(DATES),
               'modifier': rng.choice(MODS), 'quantity': rng.choice(QTY), 'unit_price': rng.choice([100, 50, 12.5, None]),
               'net_amount': rng.choice([100, 200, 300, 250.0, None, 'z']), 'authorization_id': rng.choice([None, None, 'A1', 'A2', '', 3])}
        if rng.random() < 0.05:
            row = rng.choice([None, 5, 'x', []])
        lines.append(row)
    auths = [{'authorization_id': a, 'max_quantity': rng.choice([1, 2, 5, 10, 0, -3, None, 'q', 2.5]), 'patient_id': 'P', 'service_code': 'S', 'status': 'approved',
              'valid_from': '2026-01-01', 'valid_to': '2026-12-31'} for a in rng.sample(['A1', 'A2', 'A3'], rng.randint(0, 3))]
    if rng.random() < 0.1:
        auths.append(rng.choice([None, 'x', {}]))
    att = [{'attachment_id': 'AT', 'type': 'report', 'patient_id': 'P', 'service_code': 'S', 'service_date': '2026-03-01', 'document_status': 'signed',
            'text': rng.choice(NOTES)}] if rng.random() < 0.4 else []
    c = copy.deepcopy(w.CLEAN)
    c.update(claim_id=f'S-{n:05d}', patient_id=rng.choice(['PAT-A', 'PAT-A', 'PAT-B', None, '']), provider_id=rng.choice(['PRV-1', 'PRV-1', 'PRV-2', None]),
             submission_date=rng.choice(['2026-03-10', '2026-03-10', '2026-03-09', '2026-03-11', 'bad', None]), diagnosis_code=rng.choice(DX))
    c['lines'], c['authorizations'], c['attachments'], c['notes'] = lines, auths, att, rng.choice(NOTES)
    if rng.random() < 0.03:
        c['lines'] = rng.choice([None, 'x', 5, {}])
    return c


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--scenarios', type=int, default=4000)
    ap.add_argument('--raw', type=int, default=2000)
    ap.add_argument('--seed', type=int, default=20261010)
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    a = ap.parse_args()
    rng = random.Random(a.seed)
    claims = [scenario(rng, n) for n in range(a.scenarios)]
    for n in range(a.raw):
        c = random_claim(rng) if n % 2 else damaged_coherent_claim(rng)
        if isinstance(c, dict):
            c['claim_id'] = c.get('claim_id') if isinstance(c.get('claim_id'), str) and c['claim_id'] else f'R-{n:05d}'
            claims.append(c)
    path = ROOT / 'rust' / 'target' / 'ext_corpus.jsonl'
    path.parent.mkdir(exist_ok=True)
    path.write_text('\n'.join(json.dumps(c, ensure_ascii=False, allow_nan=False, default=str) for c in claims) + '\n', encoding='utf-8')
    loaded = [json.loads(line) for line in path.read_text(encoding='utf-8').splitlines() if line.strip()]
    hist = InMemoryHistory(loaded)
    proc = subprocess.run([a.cg, 'extensions', str(path), '--root', str(ROOT)], capture_output=True, text=True, encoding='utf-8')
    if proc.returncode:
        print('rust failed:', proc.stderr[:500])
        return 2
    rust = [json.loads(line) for line in proc.stdout.splitlines() if line.strip()]
    if len(rust) != len(loaded):
        print(f'rust returned {len(rust)} rows for {len(loaded)} claims')
        return 2
    bad, tally = 0, {}
    for c, r in zip(loaded, rust):
        errs = []
        py = evaluate_extensions(c, hist, errs)
        for x in py:
            tally[(x['rule_id'], x['status'])] = tally.get((x['rule_id'], x['status']), 0) + 1
        d = same(py, r['advisory']) or (None if sorted(errs) == sorted(r['tool_errors']) else f'tool_errors {errs} != {r["tool_errors"]}')
        if d:
            bad += 1
            if bad <= 8:
                print('DIFF', c.get('claim_id'), d)
    for rid in sorted({k[0] for k in tally}):
        print(rid, {s: tally.get((rid, s), 0) for s in ('FAIL', 'PASS', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE')})
    print(f'{len(loaded)} claims compared, {bad} differences')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
