"""End-to-end test of "ask the rulebook" against a demo server this script starts.

    python rust/tools/knowledge_test.py
"""
import json
import os
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8497
pentest.PORT = PORT
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def main():
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_knowledge_test'
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
        rev = pentest.Session('CG-2002', acc['CG-2002'])

        def ask(q):
            return rev.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote(q))
        st, _, r = ask('which services need an authorization reference')
        check('a reviewer can ask, and the answer cites a rule', st == 200 and r['answered'] and any(x['source'].startswith('rule R008') for x in r['results']), (st, str(r)[:300]))
        st, _, r = ask('highest unit price for SVC-LAB')
        check('a policy answer quotes the limit and names the table version', st == 200 and any('policy' in x['source'] and 'table version' in x['source'] for x in r['results']) and 'table_version' in r, (st, str(r)[:300]))
        st, _, r = ask('what is the weather today')
        check('an unanswerable question is refused, not guessed', st == 200 and r['answered'] is False and r['results'] == [], (st, r))
        st, _, r = ask('recommend a treatment for diabetes')
        check('clinical advice is refused', st == 200 and r['answered'] is False, (st, r))
        st, _, r = ask('')
        check('an empty question is a 422', st == 422, st)
        st, _, r = ask('x' * 400)
        check('an over-long question is a 422', st == 422, st)
        st, _, r = ask('<script>alert(1)</script> duplicate lines')
        check('markup in a question is handled as text', st == 200 and '<script>' not in json.dumps(r['results']), (st, str(r)[:200]))
        anon = pentest.Session.__new__(pentest.Session)
        try:
            req = urllib.request.Request('http://127.0.0.1:%d/api/v1/knowledge/search?q=duplicate' % PORT)
            code = urllib.request.urlopen(req, timeout=5).status
        except urllib.error.HTTPError as e:
            code = e.code
        check('without a session it is refused (401)', code == 401, code)
    finally:
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
