"""Browser-side attack check: open the real console in Chrome and look at every screen that shows text another person controls.

    python rust/tools/xss_seed.py --port 8498          # plants payloads, prints the accounts file
    python rust/tools/xss_browser.py --port 8498 --accounts <path printed above>

For each screen the script asserts:
  * none of the planted payloads ran (they all set window.__xss if executed);
  * the page holds no script without a source, no element with an event-handler attribute, no iframe, no style or link element that was
    not part of the application, and no anchor with a javascript: address;
  * the payload text IS on the screen as plain text (so the check cannot pass by showing nothing);
  * the browser reported content-security-policy violations only for things the page was told not to do (informational);
  * navigating to a hostile fragment such as #/work/<script> does not run anything.
Exit 0 only if every screen is clean.
"""
import argparse, json, re, sys, time
from pathlib import Path

import pyotp
from playwright.sync_api import sync_playwright

PAYLOAD_MARKERS = ['onerror=', 'onload=', '<script>', 'javascript:window', '<iframe', '<svg', 'constructor.constructor', '<style>', '<link']
INSPECT = """() => {
  const bad = [];
  document.querySelectorAll('script:not([src])').forEach(e => bad.push('inline script: ' + e.textContent.slice(0, 60)));
  document.querySelectorAll('iframe,object,embed,base,meta[http-equiv]').forEach(e => bad.push('element ' + e.tagName));
  document.querySelectorAll('style').forEach(e => bad.push('style element'));
  document.querySelectorAll('link').forEach(e => { if (!/style\\.css$|icon\\.svg$/.test(e.getAttribute('href') || '')) bad.push('link ' + e.getAttribute('href')); });
  document.querySelectorAll('*').forEach(e => { for (const a of e.attributes) { if (/^on/i.test(a.name)) bad.push('handler ' + e.tagName + '.' + a.name); } });
  document.querySelectorAll('a[href],form[action]').forEach(e => { const u = (e.getAttribute('href') || e.getAttribute('action') || '').trim().toLowerCase(); if (u.startsWith('javascript:') || u.startsWith('data:')) bad.push('url ' + u.slice(0, 40)); });
  document.querySelectorAll('img').forEach(e => { if (!/icon(-light)?\\.svg$/.test(e.getAttribute('src') || '')) bad.push('img ' + e.getAttribute('src')); });
  return { bad, xss: window.__xss === undefined ? null : window.__xss, text: document.body.innerText };
}"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=8498)
    ap.add_argument('--accounts', required=True)
    ap.add_argument('--seed-log', required=True, help='the output of xss_seed.py (its last line lists the planted claims)')
    ap.add_argument('--shots', help='folder for screenshots')
    a = ap.parse_args()
    acc = json.loads(Path(a.accounts).read_text())
    planted_ids = json.loads(Path(a.seed_log).read_text().strip().splitlines()[-1])['claims']
    base = f'http://127.0.0.1:{a.port}/ui/'
    results = []
    csp_events = []

    def verdict(name, page, expect_text=None):
        r = page.evaluate(INSPECT)
        ok = not r['bad'] and r['xss'] is None
        if expect_text is not None:
            seen = any(m in r['text'] for m in expect_text)
            ok = ok and seen
        results.append(ok)
        print(('PASS ' if ok else 'FAIL ') + name + ('' if ok else f'  -- {r["bad"][:3]} xss={r["xss"]} payload_visible_as_text={None if expect_text is None else any(m in r["text"] for m in expect_text)}'), flush=True)
        if a.shots:
            Path(a.shots).mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(Path(a.shots) / (re.sub(r'[^A-Za-z0-9_-]+', '_', name)[:60] + '.png')))

    with sync_playwright() as p:
        browser = p.chromium.launch(channel='chrome', headless=True)

        def new_page():
            ctx = browser.new_context()
            ctx.add_init_script("window.addEventListener('securitypolicyviolation', e => { (window.__csp = window.__csp || []).push(e.violatedDirective + ' ' + e.blockedURI); });")
            pg = ctx.new_page()
            pg.on('console', lambda m: csp_events.append(m.text) if 'Content Security Policy' in m.text else None)
            pg.on('dialog', lambda d: (results.append(False), print('FAIL a dialog opened:', d.message), d.dismiss()))
            return pg

        def sign_in(page, badge):
            page.goto(base)
            page.wait_for_selector('input[type=password]')
            page.fill('input[autocomplete=username]', badge)
            page.fill('input[type=password]', acc[badge]['password'])
            page.fill('input[autocomplete=one-time-code]', pyotp.TOTP(acc[badge]['secret']).now())
            page.click('button[type=submit]')
            for _ in range(60):  # polled: the console's policy forbids the string evaluation wait_for_function would use
                if page.evaluate('() => location.hash') not in ('', '#/login'):
                    break
                page.wait_for_timeout(250)
            page.wait_for_timeout(800)

        # ---- a reviewer works through the planted claims
        page = new_page()
        sign_in(page, 'CG-3004')
        page.goto(base + '#/inbox')
        page.wait_for_timeout(1500)
        verdict('reviewer inbox list', page)
        planted = ['#/work/' + i for i in planted_ids]
        print(f'{len(planted)} planted claims to open')
        for href in planted:
            page.goto(base + href)
            page.wait_for_timeout(1500)
            verdict('claim view ' + href.split('/')[-1], page, expect_text=PAYLOAD_MARKERS)
        page.goto(base + '#/claims')
        page.wait_for_timeout(1200)
        verdict('all claims list', page)
        for frag in ('#/work/<script>window.__xss=1</script>', '#/claim/%3Cimg%20src%3Dx%20onerror%3D%22window.__xss%3D1%22%3E', '#/nonexistent"><img src=x onerror=window.__xss=2>'):
            page.goto(base + frag)
            page.wait_for_timeout(900)
            verdict('hostile address ' + frag[:28], page)
        # ---- an administrator looks at users, the audit log and a claim trace
        page2 = new_page()
        sign_in(page2, 'CG-4004')
        page2.goto(base + '#/users')
        page2.wait_for_timeout(1500)
        verdict('user list (names are payloads)', page2, expect_text=PAYLOAD_MARKERS)
        page2.goto(base + '#/audit')
        page2.wait_for_timeout(2000)
        verdict('audit panel (failed sign-ins carry payloads)', page2)
        page2.goto(base + '#/trace/CG-XSS-IMG')
        page2.wait_for_timeout(1500)
        verdict('claim trace', page2)
        page2.goto(base + '#/queue')
        page2.wait_for_timeout(1200)
        verdict('queue overview', page2)
        page2.goto(base + '#/health')
        page2.wait_for_timeout(1500)
        verdict('health panel', page2)
        # ---- the CSP itself: an injected inline handler is refused by the browser
        page2.goto(base + '#/queue')
        blocked = page2.evaluate("""() => new Promise(res => {
            const d = document.createElement('div'); d.innerHTML = '<img src=x onerror="window.__xss=\\'injected\\'">'; document.body.appendChild(d);
            setTimeout(() => res({ xss: window.__xss === undefined ? null : window.__xss, csp: window.__csp || [] }), 800); })""")
        ok = blocked['xss'] is None and any('script-src' in c or 'img-src' in c for c in blocked['csp'])
        results.append(ok)
        print(('PASS ' if ok else 'FAIL ') + f'the browser itself refuses an injected inline handler (policy reports: {blocked["csp"][:2]})')
        browser.close()
    print(f'\n{len(results)} checks, {results.count(False)} FAILED')
    return 1 if False in results else 0


if __name__ == '__main__':
    sys.exit(main())
