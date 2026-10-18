#!/usr/bin/env python3
"""A claim written in Arabic over real HTTP, against a LOCAL DEMO server this script starts itself (docs/55).

    python rust/tools/arabic_e2e.py [--cg rust/target/release/cg.exe]

The Arabic form of a clean public claim (Arabic field names and values, Arabic-Indic digits) is accepted, and sending the English form of the SAME claim
afterwards is recognised as the same claim (identical canonical input hash), which is the end-to-end proof that the two forms read to one claim. A Hijri
date, an Arabic digit inside an identifier and an unknown Arabic field name are each refused with the field and the reason. Writes outputs/defense/arabic_e2e.json.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402
from batch_e2e import clean_claims  # noqa: E402

PORT = 8497
pentest.PORT = PORT
G = json.loads((ROOT / 'rust/crates/cg-core/data/field_glossary.json').read_text(encoding='utf-8'))
FAILED, LOG = [], []


def check(name, ok, detail=''):
    LOG.append({'check': name, 'ok': bool(ok)})
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def ar_digits(s):
    return ''.join(chr(0x660 + ord(c) - 48) if c.isdigit() else c for c in s)


def twin(scope, obj):
    out = {}
    for k, v in obj.items():
        f = G['scopes'][scope][k]
        name = f['ar'][0]
        if f.get('container') == 'object':
            v = twin('coverage', v)
        elif f.get('container') == 'array':
            sub = {'lines': 'line', 'authorizations': 'authorization'}.get(k, 'attachment')
            v = [twin(sub, x) for x in v]
        elif isinstance(v, (int, float)) and not isinstance(v, bool):
            v = ar_digits(repr(v)).replace('.', '٫')
        elif isinstance(v, str):
            en = 'currency' if k == 'currency' else f.get('enum')
            if f.get('date') or k == 'submission_date':
                v = ar_digits(v)
            elif en and v in G['enums'].get(en, {}):
                v = G['enums'][en][v][0]
        out[name] = v
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    a = ap.parse_args()
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_arabic_e2e'
    data.mkdir(parents=True, exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    proc = subprocess.Popen([a.cg, 'serve', '--demo', '--queue', '--port', str(PORT), '--data-dir', str(data)], env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), CG_DEMO_CLAIMS='0'), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(120):
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{PORT}/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        admin = pentest.Session('CG-4004', acc['CG-4004'])
        claim = clean_claims(1)[0]
        arabic = twin('claim', claim)
        check('the Arabic form really is Arabic (no English field name is left)', not (set(arabic) & set(claim)), list(arabic)[:3])
        st, _, out = admin.call('POST', '/api/v1/queue/submit', {'claims': [arabic]})
        row = (out.get('results') or [{}])[0] if isinstance(out, dict) else {}
        check('the Arabic claim is accepted', st == 200 and row.get('accepted') is True and row.get('claim_id') == claim['claim_id'], (st, out))
        st, _, out2 = admin.call('POST', '/api/v1/queue/submit', {'claims': [claim]})
        row2 = (out2.get('results') or [{}])[0] if isinstance(out2, dict) else {}
        check('the English form of the same claim is recognised as the same claim', st == 200 and row2.get('already_stored') is True and row2.get('lane') == row.get('lane'), out2)
        st, _, v = admin.call('POST', '/api/v1/queue/import/validate', {'claim': arabic})
        check('import-validate returns the canonical claim and the recorded changes', st == 200 and v.get('valid') is True and v['claim'] == claim and len(v['localization']['changes']) > 20, str(v)[:300])

        def refused(name, mutate, needle):
            c = json.loads(json.dumps(arabic))
            c = mutate(c) or c
            c['رقم المطالبة'] = 'CG-AR-' + name[:6].upper().replace(' ', '')
            st, _, o = admin.call('POST', '/api/v1/queue/submit', {'claims': [c]})
            r = (o.get('results') or [{}])[0] if isinstance(o, dict) else {}
            check(name, st == 200 and r.get('accepted') is False and needle in r.get('reason', ''), (st, o))
            return r.get('reason')
        reasons = {}
        reasons['hijri'] = refused('a Hijri date is refused', lambda c: c.update({'تاريخ التقديم': '١٨ رمضان ١٤٤٧ هـ'}), 'Hijri')
        reasons['identifier'] = refused('an Arabic digit inside an identifier is refused', lambda c: c.update({'رقم المريض': 'PAT-٠٠٧'}), 'identifier')
        reasons['field'] = refused('an unknown Arabic field name is refused', lambda c: c.update({'حقل غير معروف': 1}), 'not a field name')
        reasons['ambiguous'] = refused('an ambiguous day/month date is refused', lambda c: c.update({'تاريخ التقديم': '٠٣/٠٤/٢٠٢٦'}), 'ambiguous')
        o = ROOT / 'outputs' / 'defense' / 'arabic_e2e.json'
        o.parent.mkdir(parents=True, exist_ok=True)
        o.write_text(json.dumps({'checks': LOG, 'refusal_reasons': reasons}, indent=1, ensure_ascii=False), encoding='utf-8')
        print('wrote', o)
    finally:
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
