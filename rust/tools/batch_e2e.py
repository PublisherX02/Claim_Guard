#!/usr/bin/env python3
"""Batches over real HTTP, against a LOCAL DEMO server this script starts itself (docs/53).

    python rust/tools/batch_e2e.py [--cg rust/target/release/cg.exe]

An administrator switches auto-clear on (5% audit) and submits one batch of 25 clean claims. The system clears all but the audit sample; an officer who
finds the sampled claim wrong must name the rule, and the whole batch is held; the administrator sees only counts and categories, sends claims back,
and the held batch is released once the officers have looked at them. Writes outputs/defense/batch_e2e.json (force-add: outputs/ is ignored).
"""
import argparse
import json
import os
import secrets
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402

PORT = 8496
pentest.PORT = PORT
FAILED = []
LOG = []


def check(name, ok, detail=''):
    LOG.append({'check': name, 'ok': bool(ok)})
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def clean_claims(n):
    claims = [json.loads(l) for l in (ROOT / 'data/validation/claims.jsonl').read_text(encoding='utf-8').splitlines() if l.strip()]
    expected = [json.loads(l) for l in (ROOT / 'data/validation/expected_results.jsonl').read_text(encoding='utf-8').splitlines() if l.strip()]
    bad = {e['claim_id'] for e in expected if e['status'] in ('FAIL', 'UNABLE_TO_ASSESS')}
    return [c for c in claims if c['claim_id'] not in bad][:n]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    a = ap.parse_args()
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_batch_e2e'
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
        rev = {b: pentest.Session(b, acc[b]) for b in ('CG-2002', 'CG-2003', 'CG-3003', 'CG-3004')}
        st, _, cfg = admin.call('GET', '/api/v1/queue/config')
        version = cfg.get('stored_version', 0) if isinstance(cfg, dict) else 0
        st, _, _ = admin.call('PATCH', '/api/v1/queue/config', {'expected_version': version, 'auto_clear_green': True, 'audit_percent': 5})
        check('the administrator switches auto-clear on at a 5% audit', st == 200, st)

        claims = clean_claims(25)
        ours = {c['claim_id'] for c in claims}
        check('25 clean claims are available for the batch', len(claims) == 25)
        st, _, out = admin.call('POST', '/api/v1/queue/submit', {'claims': claims})
        check('the batch is accepted', st == 200 and all(r.get('accepted') for r in out.get('results', [])), out)

        batch = None
        for _ in range(120):
            st, _, b = admin.call('GET', '/api/v1/queue/batches')
            rows = [x for x in b.get('batches', []) if x['claims'] == 25 and x['green'] == 25] if isinstance(b, dict) else []
            if rows:
                batch = rows[0]
                break
            time.sleep(0.5)
        check('the administrator sees the batch', batch is not None, b)
        bid = batch['batch_id']
        check('the sample is at least one and about 5% of the clean claims', batch['sampled'] == max(1, -(-batch['green'] * 5 // 100)) and batch['sampled'] >= 1, batch)
        check('nothing is wrong yet', batch['status'] == 'open' and batch['sample_flagged'] == 0, batch)

        # wait until the sampled claims are in an officer's inbox
        mine = None
        for _ in range(200):
            for badge, s in rev.items():
                st, _, inbox = s.call('GET', '/api/v1/work/inbox')
                if st == 200:
                    for v in (inbox if isinstance(inbox, list) else inbox.get('claims', inbox.get('items', []))):
                        if v.get('in_batch') and v.get('allowed_actions') and v['claim_id'] in ours:
                            mine = (badge, v)
                            break
                if mine:
                    break
            if mine:
                break
            time.sleep(0.5)
        check('the sampled claim reaches an officer', mine is not None)
        badge, view = mine
        s = rev[badge]
        cid = view['claim_id']
        # the officer must say which rule should have caught it
        st, _, _ = s.call('POST', f'/api/v1/work/claims/{cid}/verify', {'action': 'escalate'})
        check('flagging without a rule is refused', st == 422, st)
        st, _, _ = s.call('POST', f'/api/v1/work/claims/{cid}/verify', {'action': 'escalate', 'category': 'R99'})
        check('flagging with an unknown rule is refused', st == 422, st)
        st, _, _ = s.call('GET', f'/api/v1/queue/batches/{bid}')
        check('an officer cannot read a batch', st == 403, st)
        st, _, out = s.call('POST', f'/api/v1/work/claims/{cid}/verify', {'action': 'escalate', 'category': 'R007', 'note': 'amount looks wrong'})
        check('flagging with a rule is accepted', st == 200, out)

        st, _, b = admin.call('GET', f'/api/v1/queue/batches/{bid}')
        check('the whole batch is held', st == 200 and b['status'] == 'quarantined' and b['held'] is True and b['sample_flagged'] == 1 and b['categories'] == {'R007': 1}, b)
        text = json.dumps(b)
        check('the administrator sees no claim id, no note and no officer', 'amount looks wrong' not in text and cid not in text and badge not in text)

        st, _, out = admin.call('POST', f'/api/v1/queue/batches/{bid}/decision', {'action': 'review_n', 'n': 3, 'note': 'check a few'})
        check('the administrator sends 3 claims back to officers', st == 200 and out['status'] == 'in_review' and out['review']['sent_back'] == 3, out)
        st, _, out = admin.call('POST', f'/api/v1/queue/batches/{bid}/decision', {'action': 'release'})
        check('release is refused while those claims are with officers', st == 409, (st, out))

        # officers look at the claims sent back
        done = 0
        for _ in range(200):
            for badge2, s2 in rev.items():
                st, _, inbox = s2.call('GET', '/api/v1/work/inbox')
                for v in (inbox if isinstance(inbox, list) else inbox.get('claims', inbox.get('items', []))):
                    if v.get('in_batch') and v.get('allowed_actions') and v['claim_id'] in ours and v['claim_id'] != cid:
                        st, _, _ = s2.call('POST', f"/api/v1/work/claims/{v['claim_id']}/verify", {'action': 'verify_clear'})
                        done += int(st == 200)
            st, _, b = admin.call('GET', f'/api/v1/queue/batches/{bid}')
            if b['status'] == 'review_done':
                break
            time.sleep(0.5)
        check('after the officers have looked, the review is finished', b['status'] == 'review_done' and b['review']['still_with_officers'] == 0, b)
        st, _, out = admin.call('POST', f'/api/v1/queue/batches/{bid}/decision', {'action': 'release', 'note': 'looked at, understood'})
        check('the administrator releases the batch', st == 200 and out['status'] == 'released' and out['held'] is False, out)
        st, _, tr = admin.call('GET', f'/api/v1/audit/trace/{cid}')
        report = {'batch': {k: out[k] for k in ('claims', 'green', 'sampled', 'sample_flagged', 'categories', 'review', 'status')}, 'checks': LOG}
        o = ROOT / 'outputs' / 'defense' / 'batch_e2e.json'
        o.parent.mkdir(parents=True, exist_ok=True)
        o.write_text(json.dumps(report, indent=1), encoding='utf-8')
        print('wrote', o)
    finally:
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
