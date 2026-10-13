"""Differential test: the Rust ingestion (`cg ingest`) against the Python reference (`src/ingest.py`).

    python rust/tools/compare_ingest.py [--mutants 3000] [--seed 20261012]

1. The three public splits, in every format the pack ships: normalized JSONL, FHIR bundles, the relational CSV folder. Every accepted claim must be
   identical, every quarantine must agree, and the reports (warnings, carried encounters) must be identical.
2. FHIR bundles damaged at random (a value deleted, duplicated or replaced by junk, up to three times per bundle): the Rust adapter must reach the same
   verdict as Python (accept or quarantine) and, when both accept, the same claim and report. Rust may only differ in one documented way, in the safe
   direction: it refuses a bundle whose pre-authorization list is a string or an object, which Python would silently iterate.
3. CSV folders damaged in the ways the Python stress test uses (a text cell in a numeric column, a byte-order mark, an orphan row, a repeated claim id,
   a quoted multi-line cell, an Arabic-Indic digit, an extra cell, a missing column or file).
4. Odd JSONL: byte-order mark, CRLF, an invalid UTF-8 line, a JSON array, lines that are not objects.
Exit code 1 on any unexplained difference.
"""
import argparse
import copy
import csv
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
from ingest import ingest as py_ingest, summarize as py_summarize  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
JUNK = [None, '', 0, -1, 1.5, True, [], {}, 'x', '2026-02-30', {'reference': 'Patient/nope'}, [None], ['a'], {'a': {'b': {}}}, '9' * 50, 10 ** 30, 'line break']
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:600]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def rust(path, fmt=None):
    d = Path(tempfile.mkdtemp(prefix='cgi_'))
    cmd = [CG, 'ingest', str(path), '--output', str(d / 'out.jsonl'), '--quarantine', str(d / 'q.jsonl'), '--items', str(d / 'items.jsonl'), '--report', str(d / 'report.json')]
    if fmt:
        cmd += ['--format', fmt]
    p = subprocess.run(cmd, capture_output=True, text=True, encoding='utf-8', env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)))
    if p.returncode != 0:
        shutil.rmtree(d, ignore_errors=True)
        return None, p.stderr
    nl = chr(10)   # not splitlines(): that would also split inside a claim at U+2028
    items = [json.loads(l) for l in (d / 'items.jsonl').read_text(encoding='utf-8').split(nl) if l.strip()]
    claims = [json.loads(l) for l in (d / 'out.jsonl').read_text(encoding='utf-8').split(nl) if l.strip()]
    summary = json.loads((d / 'report.json').read_text(encoding='utf-8'))
    shutil.rmtree(d, ignore_errors=True)
    # re-attach each accepted claim to its item, in order
    it = iter(claims)
    for i in items:
        i['claim'] = next(it) if i['accepted'] else None
    return items, summary


def python(path, fmt=None):
    items = list(py_ingest(path, fmt))
    return items, py_summarize(items)


def compare(label, py_items, rs_items, allow_safer=None):
    """-> (differences, safer_differences)"""
    diffs, safer = [], []
    if len(py_items) != len(rs_items):
        return [f'{label}: {len(py_items)} Python items, {len(rs_items)} Rust items'], []
    for n, (p, r) in enumerate(zip(py_items, rs_items)):
        pa, ra = p.accepted, r['accepted']
        if pa != ra:
            (safer if (allow_safer and not ra and pa and allow_safer(p)) else diffs).append(f'{label}[{n}] python accepted={pa} rust accepted={ra}: py_error={p.error!r} rs_error={r["error"]!r}')
            continue
        if pa:
            if json.loads(json.dumps(p.claim)) != r['claim']:
                diffs.append(f'{label}[{n}] claim differs')
            elif p.report != r['report']:
                diffs.append(f'{label}[{n}] report differs: {json.dumps(p.report)[:300]} vs {json.dumps(r["report"])[:300]}')
        else:
            if (p.error or '').split(':')[0] != (r['error'] or '').split(':')[0] and not (allow_safer and allow_safer(p)):
                diffs.append(f'{label}[{n}] quarantine reason differs: {p.error!r} vs {r["error"]!r}')
    return diffs, safer


def sites(o):
    if isinstance(o, dict):
        for k, v in list(o.items()):
            yield o, k
            yield from sites(v)
    elif isinstance(o, list):
        for i, v in enumerate(o):
            yield o, i
            yield from sites(v)


def mutate(b, rng):
    b = copy.deepcopy(b)
    for _ in range(rng.choice((1, 1, 2, 3))):
        found = list(sites(b))
        if not found:
            break
        parent, key = rng.choice(found)
        roll = rng.random()
        if roll < 0.3:
            del parent[key]
        elif roll < 0.4 and isinstance(parent, list):
            parent.append(copy.deepcopy(parent[key]))
        else:
            parent[key] = copy.deepcopy(rng.choice(JUNK))
    return b


def prearth_string_or_object(item):
    """The documented safer difference: Python would iterate a string / object preAuthRef as if it were a list."""
    raw = item.raw
    try:
        for e in raw.get('entry', []):
            for ins in (e.get('resource') or {}).get('insurance', []) or []:
                if isinstance(ins, dict) and isinstance(ins.get('preAuthRef'), (str, dict)):
                    return True
    except Exception:  # noqa: BLE001
        return False
    return False


def public_splits():
    for split in ('stress', 'validation', 'development'):
        d = ROOT / 'data' / split
        for name, fmt in (('claims.jsonl', None), ('fhir_bundles.jsonl', None)):
            py, _ = python(d / name, fmt)
            rs, _ = rust(d / name, fmt)
            diffs, _ = compare(f'{split}/{name}', py, rs)
            check(f'{split}/{name}: {len(py)} records, same verdicts, claims and reports', not diffs, diffs[:3])
        py, _ = python(d / 'csv')
        rs, _ = rust(d / 'csv')
        diffs, _ = compare(f'{split}/csv', py, rs)
        check(f'{split}/csv: {len(py)} records, same verdicts, claims and reports', not diffs, diffs[:3])


def fhir_fuzz(n, seed):
    bundles = [json.loads(l) for l in (ROOT / 'data/development/fhir_bundles.jsonl').read_text(encoding='utf-8').splitlines()[:80]]
    rng = random.Random(seed)
    mutants = [mutate(rng.choice(bundles), rng) for _ in range(n)]
    tmp = Path(tempfile.mkdtemp(prefix='cgi_fz_'))
    f = tmp / 'mutants.jsonl'
    f.write_text(''.join(json.dumps(m) + '\n' for m in mutants), encoding='utf-8')
    py, _ = python(f, 'fhir_bundle')
    rs, _ = rust(f, 'fhir_bundle')
    diffs, safer = compare('fhir-mutants', py, rs, allow_safer=prearth_string_or_object)
    acc = sum(1 for p in py if p.accepted)
    check(f'{n} damaged bundles: Python accepted {acc}, quarantined {n - acc}; Rust reached the same verdict, claim and report on every one', not diffs, diffs[:5])
    print(f'     ({len(safer)} bundles with a string/object preAuthRef that only Rust refuses: documented, safe direction)')
    # whatever Rust accepts must also pass the engine; run the accepted ones through the Rust engine and confirm 15 results each, no error
    ok = [json.dumps(r['claim']) for r in rs if r['accepted']]
    (tmp / 'accepted.jsonl').write_text('\n'.join(ok) + '\n', encoding='utf-8')
    p = subprocess.run([CG, 'evaluate', str(tmp / 'accepted.jsonl')], capture_output=True, text=True, encoding='utf-8', env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)))
    rows = [json.loads(l) for l in p.stdout.splitlines() if l.strip()]
    check(f'the {len(ok)} accepted damaged claims all reach the engine and get fifteen results', p.returncode == 0 and len(rows) == len(ok) and all(len(r['results']) == 15 for r in rows), p.stderr[-200:])
    shutil.rmtree(tmp, ignore_errors=True)


def fhir_handmade():
    """Shapes the public pack never contains: Encounter resources, dangling references, undecodable attachments, odd statuses."""
    import base64
    every = [json.loads(l) for l in (ROOT / 'data/development/fhir_bundles.jsonl').read_text(encoding='utf-8').split(chr(10)) if l.strip()]
    base = next(b for b in every if any(e['resource']['resourceType'] == 'DocumentReference' for e in b['entry']))   # one with attachments
    cases = {}

    def entry(b, rtype):
        return [e for e in b['entry'] if e['resource']['resourceType'] == rtype]

    def variant(name, fn):
        b = copy.deepcopy(base)
        fn(b)
        cases[name] = b

    patient_url = entry(base, 'Patient')[0]['fullUrl']
    enc = lambda eid, subj: {'fullUrl': 'urn:enc:' + eid, 'resource': {'resourceType': 'Encounter', 'id': eid, 'status': 'finished', 'class': {'code': 'AMB'}, 'subject': {'reference': subj},
                                                                      'serviceProvider': {'reference': 'Organization/x'}, 'period': {'start': '2026-01-02', 'end': '2026-01-03'}}}
    variant('an encounter for the same patient', lambda b: b['entry'].append(enc('E1', patient_url)))
    variant('an encounter for another patient', lambda b: b['entry'].append(enc('E2', 'Patient/other')))
    variant('an item pointing at an encounter that is not there', lambda b: entry(b, 'Claim')[0]['resource']['item'][0].update(encounter=[{'reference': 'Encounter/ghost'}]))
    variant('an item pointing at an encounter that is there', lambda b: (b['entry'].append(enc('E3', patient_url)), entry(b, 'Claim')[0]['resource']['item'][0].update(encounter=[{'reference': 'urn:enc:E3'}])))
    docs = entry(base, 'DocumentReference')
    if docs:
        def bad64(b):
            entry(b, 'DocumentReference')[0]['resource']['content'][0]['attachment']['data'] = 'not base64 !!'
        variant('an attachment that is not base64', bad64)
        variant('an attachment with valid base64 that is not UTF-8', lambda b: entry(b, 'DocumentReference')[0]['resource']['content'][0]['attachment'].update(data=base64.b64encode(bytes([255, 254, 0])).decode()))
        variant('an unknown docStatus', lambda b: entry(b, 'DocumentReference')[0]['resource'].update(docStatus='amended'))
        variant('a missing docStatus', lambda b: entry(b, 'DocumentReference')[0]['resource'].pop('docStatus', None))
        variant('a description that is not the synthetic pattern', lambda b: entry(b, 'DocumentReference')[0]['resource'].update(description='Lab report'))
    variant('a supportingInfo link with no document', lambda b: entry(b, 'Claim')[0]['resource'].setdefault('supportingInfo', []).append({'valueReference': {'reference': 'DocumentReference/missing'}}))
    variant('two claims', lambda b: b['entry'].append(copy.deepcopy(entry(b, 'Claim')[0])))
    variant('no claim', lambda b: b.update(entry=[e for e in b['entry'] if e['resource']['resourceType'] != 'Claim']))
    variant('a sequence of 7', lambda b: entry(b, 'Claim')[0]['resource']['item'][0].update(sequence=7))
    variant('a float sequence', lambda b: entry(b, 'Claim')[0]['resource']['item'][0].update(sequence=2.0))
    variant('a string sequence', lambda b: entry(b, 'Claim')[0]['resource']['item'][0].update(sequence='A-1'))
    variant('no currency anywhere', lambda b: (entry(b, 'Claim')[0]['resource'].pop('total', None), [i.pop('unitPrice', None) for i in entry(b, 'Claim')[0]['resource']['item']]))
    tmp = Path(tempfile.mkdtemp(prefix='cgi_hm_'))
    f = tmp / 'handmade.jsonl'
    names = list(cases)
    f.write_text(''.join(json.dumps(cases[n]) + chr(10) for n in names), encoding='utf-8')
    py, _ = python(f, 'fhir_bundle')
    rs, _ = rust(f, 'fhir_bundle')
    diffs, _ = compare('fhir-handmade', py, rs)
    check(f'{len(names)} hand-made FHIR cases (encounters, dangling references, bad attachments, odd ids): identical', not diffs, diffs[:5])
    for n, p in zip(names, py):
        print('     %-62s %s' % (n, 'accepted' + (' with %d warning(s)' % len(p.report.get('warnings', [])) if p.accepted else '') if p.accepted else 'quarantined'))
    shutil.rmtree(tmp, ignore_errors=True)


def read_csv(p):
    with open(p, newline='', encoding='utf-8') as f:
        rows = list(csv.DictReader(f))
        return rows, list(rows[0].keys()) if rows else None


def write_csv(p, rows, fields):
    with open(p, 'w', newline='', encoding='utf-8') as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        w.writerows(rows)


def csv_cases():
    src = ROOT / 'data' / 'development' / 'csv'
    rng = random.Random(7)
    base_ids = [r['claim_id'] for r in read_csv(src / 'claims.csv')[0]][:30]
    cases = {}

    def fresh():
        d = Path(tempfile.mkdtemp(prefix='cgi_csv_'))
        for name in ('claims', 'lines', 'coverage', 'authorizations', 'attachments'):
            rows, fields = read_csv(src / f'{name}.csv')
            if fields is None:
                fields = list(csv.DictReader(open(src / f'{name}.csv', encoding='utf-8')).fieldnames)
            write_csv(d / f'{name}.csv', [r for r in rows if r['claim_id'] in base_ids], fields)
        return d

    d = fresh(); cases['clean'] = d
    d = fresh(); rows, f = read_csv(d / 'lines.csv'); rows[3]['quantity'] = 'two'; rows[5]['unit_price'] = '1,5'; rows[7]['net_amount'] = '١٢'; write_csv(d / 'lines.csv', rows, f); cases['text and Arabic-Indic digits in numeric cells'] = d
    d = fresh(); (d / 'claims.csv').write_bytes(b'\xef\xbb\xbf' + (d / 'claims.csv').read_bytes()); cases['byte-order mark on claims.csv'] = d
    d = fresh(); rows, f = read_csv(d / 'lines.csv'); rows.append({**rows[0], 'claim_id': 'CG-ORPHAN'}); write_csv(d / 'lines.csv', rows, f); cases['orphan row'] = d
    d = fresh(); rows, f = read_csv(d / 'claims.csv'); rows.append(dict(rows[2])); write_csv(d / 'claims.csv', rows, f); cases['repeated claim id'] = d
    d = fresh(); rows, f = read_csv(d / 'claims.csv'); rows[1]['notes'] = 'line one\nline "two", with comma'; write_csv(d / 'claims.csv', rows, f); cases['quoted multi-line cell'] = d
    d = fresh(); t = (d / 'lines.csv').read_text(encoding='utf-8').splitlines(); t[2] += ',extra'; (d / 'lines.csv').write_text('\n'.join(t) + '\n', encoding='utf-8'); cases['an extra cell on a line row'] = d
    d = fresh(); rows, f = read_csv(d / 'claims.csv'); f.remove('total_amount'); write_csv(d / 'claims.csv', [{k: v for k, v in r.items() if k != 'total_amount'} for r in rows], f); cases['no total_amount column'] = d
    d = fresh(); (d / 'coverage.csv').unlink(); cases['coverage.csv missing'] = d
    d = fresh(); (d / 'attachments.csv').write_bytes(b'\xff\xfe garbage'); cases['attachments.csv is not UTF-8'] = d
    d = fresh(); rows, f = read_csv(d / 'claims.csv'); rows[0]['submission_date'] = '2026-13-45'; rows[1]['claim_id'] = ''; rows[2]['total_amount'] = '1_000.50'; rows[3]['total_amount'] = 'inf'; write_csv(d / 'claims.csv', rows, f); cases['bad date, empty id, underscore number, inf'] = d
    d = fresh(); rows, f = read_csv(d / 'lines.csv'); [r.update(quantity=rng.choice(['1', '1.0', '0', '-1', '1e2', ' 3 ', '2.50', '', '٣'])) for r in rows]; write_csv(d / 'lines.csv', rows, f); cases['varied number spellings'] = d
    return cases


def csv_fuzz():
    cases = csv_cases()
    for name, d in cases.items():
        try:
            py, _ = python(d)
        except Exception as e:  # noqa: BLE001  Python crashes on some of these; Rust must not
            rs, _ = rust(d)
            check(f'csv: {name}: Python raised {type(e).__name__}; Rust did not abort', rs is not None)
            continue
        rs, summary = rust(d)
        if rs is None:
            check(f'csv: {name}', False, summary)
            continue
        diffs, _ = compare(f'csv/{name}', py, rs)
        check(f'csv: {name}: {sum(i.accepted for i in py)} accepted, {sum(not i.accepted for i in py)} quarantined, identical', not diffs, diffs[:3])
    for d in cases.values():
        shutil.rmtree(d, ignore_errors=True)


def jsonl_cases():
    good = (ROOT / 'data/development/claims.jsonl').read_bytes().splitlines()[:6]
    cases = {
        'byte-order mark and CRLF': b'\xef\xbb\xbf' + b'\r\n'.join(good) + b'\r\n',
        'an invalid UTF-8 line between good ones': good[0] + b'\n' + b'{"claim_id": "\xff\xfe"}\n' + good[1] + b'\n',
        'blank lines and non-objects': good[0] + b'\n\n   \n"a string"\n[1,2]\nnull\n42\n' + good[1] + b'\n',
        'one JSON array of claims': b'[' + b','.join(good[:3]) + b']',
        'a broken array': b'[' + good[0] + b', {"x": ',
        'not JSON at all': b'this is not json\n' + good[0] + b'\n',
        'U+2028 inside a string': good[0].replace(b'"notes": "', b'"notes": "a\xe2\x80\xa8b ') + b'\n',
        'a claim with a missing key': good[0].replace(b'"currency"', b'"currenxy"') + b'\n' + good[1] + b'\n',
        'empty file': b'',
    }
    return cases


def jsonl_fuzz():
    tmp = Path(tempfile.mkdtemp(prefix='cgi_jl_'))
    for name, data in jsonl_cases().items():
        f = tmp / 'case.jsonl'
        f.write_bytes(data)
        try:
            py, _ = python(f) if data else ([], None)
        except Exception as e:  # noqa: BLE001
            check(f'jsonl: {name}: Python raised {type(e).__name__}', False, e)
            continue
        rs, summary = rust(f)
        if rs is None:
            check(f'jsonl: {name}', False, summary)
            continue
        diffs, _ = compare(f'jsonl/{name}', py, rs)
        check(f'jsonl: {name}: {len(py)} records, identical', not diffs, diffs[:3])
    shutil.rmtree(tmp, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--mutants', type=int, default=3000)
    ap.add_argument('--seed', type=int, default=20261012)
    a = ap.parse_args()
    public_splits()
    fhir_handmade()
    fhir_fuzz(a.mutants, a.seed)
    fhir_fuzz(a.mutants, a.seed + 1)
    csv_fuzz()
    jsonl_fuzz()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
