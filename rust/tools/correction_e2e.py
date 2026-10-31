#!/usr/bin/env python3
"""The mentor's workflow in a real Chrome, against a LOCAL DEMO server: a claim with a wrong amount arrives, the copilot explains it, the
senior tries a correction in a what-if, applies it, and the claim is validated again as a new version; the trace shows both versions.

    cg serve --demo --queue --port 8443 --data-dir <dir>
    python rust/tools/correction_e2e.py --accounts <dir>/demo_accounts.json --out shots_fix

Needs playwright and pyotp (the repository's .venv has them). Talks only to a local demo server with its generated demo accounts.
"""
import argparse
import copy
import json
import sys
import time
from pathlib import Path

import pyotp
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]


def login(browser, base, acc, badge, last, vp=None):
    ctx = browser.new_context(viewport=vp or {'width': 1440, 'height': 1000})
    pg = ctx.new_page()
    issues = []
    pg.on('console', lambda m: issues.append(('console.' + m.type, m.text[:200])) if m.type in ('error', 'warning') else None)
    pg.on('pageerror', lambda e: issues.append(('pageerror', str(e)[:200])))
    pg.goto(base + '#/login')
    pg.wait_for_selector('input[autocomplete=username]')
    tp = pyotp.TOTP(acc[badge]['secret'])
    # the server refuses a code it already accepted, also from an earlier run: wait for the next 30-second window after a first use
    if badge not in last:
        time.sleep(31 - time.time() % 30)
    while tp.now() == last.get(badge) or 30 - time.time() % 30 < 2:
        time.sleep(1)
    last[badge] = tp.now()
    pg.fill('input[autocomplete=username]', badge)
    pg.fill('input[type=password]', acc[badge]['password'])
    pg.fill('input[autocomplete=one-time-code]', last[badge])
    pg.click('button[type=submit]')
    pg.wait_for_function("() => location.hash !== '#/login'", timeout=8000)
    return ctx, pg, issues


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--base', default='http://127.0.0.1:8443/')
    ap.add_argument('--accounts', required=True)
    ap.add_argument('--out', default='shots_fix')
    a = ap.parse_args()
    if a.base.split('//', 1)[-1].split('/', 1)[0].split(':')[0] not in ('127.0.0.1', 'localhost', '::1'):
        print('refusing: local demo server only')
        return 2
    acc = json.load(open(a.accounts, encoding='utf-8'))
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    claims = [json.loads(l) for l in (ROOT / 'data/stress/claims.jsonl').read_text(encoding='utf-8').splitlines() if l.strip()]
    flagged = {json.loads(l)['claim_id'] for l in (ROOT / 'data/stress/expected_results.jsonl').read_text(encoding='utf-8').splitlines()
               if l.strip() and json.loads(l)['status'] in ('FAIL', 'UNABLE_TO_ASSESS')}
    base_claim = copy.deepcopy(next(c for c in claims if c['claim_id'] not in flagged))   # a claim the answer key calls clean
    base_claim['claim_id'] = 'E2E-FIX-%d' % int(time.time() % 100000)
    ln = base_claim['lines'][0]
    ln['net_amount'] = ln['quantity'] * ln['unit_price'] + 11
    cid = base_claim['claim_id']
    last, issues, checks = {}, [], []

    def check(name, ok, extra=''):
        checks.append((name, bool(ok)))
        print(('PASS ' if ok else 'FAIL ') + name + (' ' + str(extra) if extra and not ok else ''))

    with sync_playwright() as p:
        browser = p.chromium.launch(channel='chrome', headless=True)
        # 1. an administrator submits the claim; the senior is put on shift so it is dealt to them
        ctx, pg, iss = login(browser, a.base, acc, 'CG-4004', last)
        cfg = pg.evaluate("() => CG.api('GET', '/queue/config')")
        version = cfg.get('stored_version', 0)
        pg.evaluate("(v) => CG.api('PATCH', '/queue/config', { expected_version: v, on_shift: ['CG-3003'], slice_size: 100, low_water: 99 })", version)
        res = pg.evaluate("(c) => CG.api('POST', '/queue/submit', { claims: [c] })", base_claim)
        check('the wrong claim is accepted', res['claims'][0]['accepted'] if 'claims' in res else res, res)
        issues += iss
        ctx.close()
        # 2. the senior opens it
        ctx, pg, iss = login(browser, a.base, acc, 'CG-3003', last)
        found = False
        for _ in range(25):
            pg.goto(a.base + '#/inbox')
            pg.wait_for_timeout(1500)
            d = pg.evaluate("() => CG.api('POST', '/work/next')")
            if d and d.get('claim') and d['claim']['claim_id'] == cid:
                found = True
                break
            inbox = pg.evaluate("() => CG.api('GET', '/work/inbox')")
            if any(c['claim_id'] == cid for c in inbox['claims']):
                found = True
                break
            time.sleep(2)
        check('the claim reaches the senior', found)
        if not found:
            return 1
        pg.goto(a.base + '#/work/' + cid)
        pg.wait_for_selector('.copilot', timeout=10000)
        pg.wait_for_timeout(500)
        pg.screenshot(path=str(out / '1_copilot.png'), full_page=True)
        text = pg.text_content('.copilot')   # the step-by-step list is collapsed, and inner_text leaves out what is hidden
        check('the copilot names R007 and R012', 'R007' in text and 'R012' in text, text[:200])
        check('the copilot offers a what-if for the proposed amounts', pg.locator('.copilot button:has-text("Fill in all")').count() >= 1)
        # 3. try the proposed correction in a what-if
        pg.locator('.copilot button:has-text("Fill in all")').first.click()
        pg.wait_for_selector('.fixpanel table', timeout=10000)
        pg.wait_for_timeout(800)
        pg.screenshot(path=str(out / '2_whatif.png'), full_page=True)
        res_text = pg.inner_text('.fixpanel')
        check('the what-if says every check would pass', 'every check would pass' in res_text, res_text[-300:])
        after = pg.evaluate("(c) => CG.api('GET', '/work/inbox').then(d => d.claims.filter(x => x.claim_id === c)[0].version)", cid)
        check('the what-if saved nothing (still version 1)', after == 1, after)
        # 4. apply and recheck
        pg.fill('.fixpanel textarea', 'Line amount and total corrected from the bill.')
        pg.check('#fixack')
        pg.click('.fixpanel button:has-text("Apply the correction")')
        pg.wait_for_selector('.fixpanel .notice:has-text("Rechecked as a new version")', timeout=10000)
        pg.screenshot(path=str(out / '3_rechecked.png'), full_page=True)
        pg.wait_for_timeout(2500)
        issues += iss
        ctx.close()
        # 5. the administrator reads the trace
        ctx, pg, iss = login(browser, a.base, acc, 'CG-4004', last)
        tr = pg.evaluate("(c) => CG.api('GET', '/ops/trace/' + c)", cid)
        vs = tr['versions']
        check('the trace has both versions', len(vs) == 2, [v['version'] for v in vs])
        check('the original is closed as rechecked', vs[0]['state'] == 'rechecked', vs[0]['state'])
        check('the new version has no findings', vs[1]['findings'] == [], vs[1]['findings'])
        kinds = [e['event']['event_type'] for e in tr['security_log']]
        check('the security log has what-if and recheck', 'claim_whatif' in kinds and 'claim_rechecked' in kinds, kinds)
        issues += iss
        ctx.close()
        browser.close()
    check('no console errors or warnings', not issues, issues[:3])
    bad = [n for n, ok in checks if not ok]
    print('\n%d checks, %d failed' % (len(checks), len(bad)))
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
