"""End-to-end test of editing the payer rules, against a demo server this script starts (the demo has two administrators, so dual control applies).

    python rust/tools/payer_test.py

Checks: only administrators see the rules; a bad table is refused with reasons and changes nothing; a good change is NOT applied when asked for, it waits for a
second administrator; the requester cannot approve it; the second administrator approves with a fresh authenticator code; then the table is version 1, the
rulebook hash has changed, the history names both people and the reason, and the claim the old limit refused is now judged by the new one; the history is a
verifying hash chain; a rollback is a new version. Authorized scope: the user's own demo server on 127.0.0.1.
"""
import copy
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8498
pentest.PORT = PORT
FAILED = []
REASON = 'contract amendment 2026-10: laboratory unit price ceiling raised'


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def fresh_code(tp, used):
    """the next one-time code that has not been used yet (the server refuses a replay)"""
    while tp.now() in used or 30 - time.time() % 30 < 3:
        time.sleep(1)
    used.add(tp.now())
    return tp.now()


def main():
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_payer_test'
    data.mkdir(parents=True, exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    proc = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen('http://127.0.0.1:%d/healthz' % PORT, timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        a1 = pentest.Session('CG-4004', acc['CG-4004'])
        a2 = pentest.Session('CG-4005', acc['CG-4005'])
        tp2 = pyotp.TOTP(acc['CG-4005']['secret'])
        used2 = {tp2.now()}      # the code a2 just signed in with
        rev = pentest.Session('CG-2002', acc['CG-2002'])
        check('a reviewer cannot read or change the payer rules', rev.call('GET', '/api/v1/rules/payer')[0] == 403 and rev.call('POST', '/api/v1/rules/payer', {'policies': {}, 'reason': REASON})[0] == 403)
        st, _, cur = a1.call('GET', '/api/v1/rules/payer')
        check('an administrator reads the table in force: version 0, the shipped one', st == 200 and cur['version'] == 0 and len(cur['policies']) >= 2, (st, str(cur)[:200]))
        h0 = cur['rulebook_hash']
        table = copy.deepcopy(cur['policies'])
        pid = sorted(table)[0]
        # a bad table
        bad = copy.deepcopy(table)
        bad[pid]['max_unit_price']['SVC-GHOST'] = -5
        bad[pid]['allowed_providers'] = []
        st, _, v = a1.call('POST', '/api/v1/rules/payer/validate', {'policies': bad})
        check('a bad table is explained: unknown service, negative price, empty provider list', st == 200 and v['ok'] is False and len(v['errors']) >= 3, v)
        st, _, r = a1.call('POST', '/api/v1/rules/payer', {'policies': bad, 'reason': REASON})
        check('and it is refused, not queued for approval', st == 422, (st, r))
        # a good change
        good = copy.deepcopy(table)
        good[pid]['max_unit_price']['SVC-LAB'] = 400
        st, _, v = a1.call('POST', '/api/v1/rules/payer/validate', {'policies': good})
        check('the change is described as a list of differences', st == 200 and v['ok'] and any('SVC-LAB' in c and '400' in c for c in v['changes']), v)
        st, _, r = a1.call('POST', '/api/v1/rules/payer', {'policies': good, 'reason': 'short'})
        check('a reason of 10 characters or more is required', st == 422, (st, r))
        st, _, r = a1.call('POST', '/api/v1/rules/payer', {'policies': good, 'reason': REASON})
        check('asking for the change does NOT apply it: it waits for a second administrator (202)', st == 202 and r['status'] == 'pending_approval', (st, r))
        ap = r['approval']['id']
        st, _, now = a1.call('GET', '/api/v1/rules/payer')
        check('the table is still version 0', now['version'] == 0 and now['rulebook_hash'] == h0)
        code = pyotp.TOTP(acc['CG-4004']['secret']).now()
        st, _, r = a1.call('POST', '/api/v1/approvals/%s/approve' % ap, {'totp_code': code})
        check('the person who asked cannot approve their own request', st == 403, (st, r))
        st, _, r = a2.call('POST', '/api/v1/approvals/%s/approve' % ap, {'totp_code': fresh_code(tp2, used2)})
        check('a different administrator approves with a fresh authenticator code', st == 200, (st, r))
        st, _, now = a1.call('GET', '/api/v1/rules/payer')
        check('the table is now version 1 and the rulebook hash changed', now['version'] == 1 and now['rulebook_hash'] != h0 and now['policies'][pid]['max_unit_price']['SVC-LAB'] == 400, (now['version'], now['rulebook_hash'] == h0))
        last = now['history'][-1]
        check('the history names who asked, who approved and why', last['actor'] == 'CG-4004' and last['approver'] == 'CG-4005' and last['reason'] == REASON and last['changes'], last)
        # the engine uses it: a claim at 300 per laboratory unit was over the shipped limit of 260
        claims = [json.loads(l) for l in (ROOT / 'data/validation/claims.jsonl').read_text(encoding='utf-8').split(chr(10)) if l.strip()]
        c = next(x for x in claims if x['policy_id'] == pid)
        c = copy.deepcopy(c)
        c['claim_id'] = 'CG-PAYERTEST01'
        c['lines'] = [{'line_id': 'L1', 'service_code': 'SVC-LAB', 'service_date': c['submission_date'], 'modifier': None, 'quantity': 1, 'unit_price': 300, 'net_amount': 300, 'authorization_id': None}]
        c['total_amount'] = 300
        c['authorizations'] = []
        st, _, res = a1.call('POST', '/api/v1/queue/submit', {'claims': [c]})
        check('a claim at 300 is accepted into the queue under version 1', st == 200 and res['results'][0]['accepted'], (st, str(res)[:200]))
        st, _, tr = a1.call('GET', '/api/v1/claims/CG-PAYERTEST01')
        r013 = next((x for x in (tr.get('results') or []) if x.get('rule_id') == 'R013'), None) if isinstance(tr, dict) else None
        if r013 is not None:
            check('and rule R013 (price limit) passes it, because the limit in force is 400', r013['status'] != 'FAIL', r013['status'])
        # rollback to version 0 is a new version
        old = [h for h in now['history'] if h['version'] == 0]
        st, _, r = a1.call('POST', '/api/v1/rules/payer', {'policies': cur['policies'], 'reason': 'rollback of version 1: the amendment was withdrawn'})
        check('a rollback goes through the same approval', st == 202, (st, r))
        ap2 = r['approval']['id']
        st, _, r = a2.call('POST', '/api/v1/approvals/%s/approve' % ap2, {'totp_code': fresh_code(tp2, used2)})
        st, _, now = a1.call('GET', '/api/v1/rules/payer')
        check('after the approved rollback it is version 2 with the shipped table and the original hash again', now['version'] == 2 and now['rulebook_hash'] == h0, (now['version'], now['rulebook_hash'] == h0))
        rc = subprocess.run([CG, 'audit-verify', str(data / 'payer_rules.jsonl')], capture_output=True, text=True, env=dict(os.environ, FERNET_KEY=json.loads((data / 'dev_secrets.json').read_text())['FERNET_KEY']))
        check('the history file is a verifying hash chain', rc.returncode == 0, rc.stdout + rc.stderr)
    finally:
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
