#!/usr/bin/env python3
"""Visual and mechanical review of the reviewer console in a real Chrome, against a LOCAL DEMO server.

    cg serve --demo --port 8765 --data-dir <dir>
    pip install playwright pyotp            # drives the installed Chrome (channel "chrome"); no browser download
    python rust/tools/console_review.py --accounts <dir>/demo_accounts.json --out shots

For each demo role it signs in (the authenticator code is computed from the generated demo seed), opens every page the role may see at desktop
(1440 px) and phone (390 px) width, opens one claim in the work view, saves full-page screenshots, and FAILS on: a console error or warning,
an HTTP error, an uncaught exception, a sign-in that does not complete, or a page that scrolls sideways. It only ever talks to the address you
give it with the demo accounts the demo server generated; it must not be pointed at a real deployment.
Note: string arguments to Playwright's evaluate/wait_for_function are eval'd and the console's CSP (rightly) blocks that, so use arrow functions.
"""
import argparse
import json
import sys
import time
from pathlib import Path

import pyotp
from playwright.sync_api import sync_playwright

ROLES = {'admin': ('CG-4004', ['#/queue', '#/inbox', '#/claims', '#/submit', '#/config', '#/rules', '#/import', '#/ask', '#/users', '#/health', '#/audit']),
         'senior': ('CG-3003', ['#/inbox', '#/claims', '#/queue', '#/ask']),
         'reviewer': ('CG-2002', ['#/inbox', '#/claims']),
         'viewer': ('CG-1001', ['#/claims'])}
VIEWPORTS = (({'width': 1440, 'height': 900}, 'desktop'), ({'width': 390, 'height': 800}, 'phone'))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--base', default='http://127.0.0.1:8765/')
    ap.add_argument('--accounts', required=True, help='demo_accounts.json written by `cg serve --demo`')
    ap.add_argument('--out', default='console_shots')
    a = ap.parse_args()
    host = a.base.split('//', 1)[-1].split('/', 1)[0].split(':')[0]
    if host not in ('127.0.0.1', 'localhost', '::1'):
        print('refusing: this tool signs in with demo accounts and is for a local demo server only')
        return 2
    acc = json.load(open(a.accounts, encoding='utf-8'))
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    issues, last_code = [], {}
    with sync_playwright() as p:
        browser = p.chromium.launch(channel='chrome', headless=True)
        for role, (badge, routes) in ROLES.items():
            for vp, vname in VIEWPORTS:
                ctx = browser.new_context(viewport=vp)
                pg = ctx.new_page()
                tag = '%s/%s' % (role, vname)
                pg.on('console', lambda m, t=tag: issues.append((t, 'console.' + m.type, m.text[:200])) if m.type in ('error', 'warning') else None)
                pg.on('pageerror', lambda e, t=tag: issues.append((t, 'pageerror', str(e)[:200])))
                pg.on('response', lambda r, t=tag: issues.append((t, 'http %d' % r.status, r.url[-80:])) if r.status >= 400 and 'auth/me' not in r.url else None)
                pg.goto(a.base + '#/login')
                pg.wait_for_selector('input[autocomplete=username]')
                tp = pyotp.TOTP(acc[badge]['secret'])
                while tp.now() == last_code.get(badge) or 30 - time.time() % 30 < 2:   # the server refuses a code it already accepted
                    time.sleep(1)
                last_code[badge] = tp.now()
                pg.fill('input[autocomplete=username]', badge)
                pg.fill('input[type=password]', acc[badge]['password'])
                pg.fill('input[autocomplete=one-time-code]', last_code[badge])
                pg.click('button[type=submit]')
                try:
                    pg.wait_for_function("() => location.hash !== '#/login'", timeout=8000)
                except Exception:
                    issues.append((tag, 'login', 'did not complete'))
                    ctx.close()
                    continue
                for r in routes:
                    pg.goto(a.base + r)
                    pg.wait_for_timeout(1800)
                    pg.screenshot(path=str(out / ('%s_%s_%s.png' % (role, vname, r.strip('#/').replace('/', '_')))), full_page=True)
                    if pg.evaluate('() => document.documentElement.scrollWidth - document.documentElement.clientWidth') > 0:
                        issues.append((tag, 'overflow', '%s scrolls sideways' % r))
                if '#/inbox' in routes and role in ('senior', 'reviewer'):
                    pg.goto(a.base + '#/inbox')
                    try:
                        pg.wait_for_selector('tbody tr', timeout=8000)
                        pg.click('tbody tr')
                        pg.wait_for_timeout(2500)
                        pg.screenshot(path=str(out / ('%s_%s_work.png' % (role, vname))), full_page=True)
                        if pg.evaluate('() => document.documentElement.scrollWidth - document.documentElement.clientWidth') > 0:
                            issues.append((tag, 'overflow', 'work view scrolls sideways'))
                    except Exception as e:
                        issues.append((tag, 'work view', str(e)[:120]))
                ctx.close()
        browser.close()
    for i in issues:
        print('ISSUE', *i)
    print('%d issue(s); screenshots in %s' % (len(issues), out))
    return 1 if issues else 0


if __name__ == '__main__':
    sys.exit(main())
