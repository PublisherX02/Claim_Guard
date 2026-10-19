"""Does the PRODUCT reproduce the measured numbers of docs/52?  (the gate for switching meaning-based "Ask the rulebook" on)

    python rust/tools/rag_parity.py

Starts the embedding sidecar (deploy/embed/service.py, CPU, offline) and a demo server whose EMBEDDER_URL points at it, then sends the frozen question sets of
experiments/rag_semantic/ through the real HTTP route (/api/v1/knowledge/search) and compares with experiments/rag_semantic/results.json
(system `dense_minilm_multi`, threshold 0.29, which was run on a GPU: this run is a CPU, in a different process, through the Rust cosine). Then it stops the
sidecar and checks that the server falls back to the keyword search and says so.

Writes outputs/defense/rag_parity.json (force-add: outputs/ is ignored). Needs rust/target/release/cg.exe, python with sentence-transformers, and the model in the
local Hugging Face cache (the sidecar runs offline).
"""
import json
import os
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
EXP = ROOT / 'experiments' / 'rag_semantic'
PORT, EPORT = 8498, 8098
pentest.PORT = PORT
FAILED = []
TOL = 1e-9


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:400]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def wait(url, headers=None, tries=240):
    for _ in range(tries):
        try:
            urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    return False


def load_rows():
    para = json.loads((EXP / 'questions_paraphrase.json').read_text(encoding='utf-8'))
    rust = json.loads((EXP / 'questions_rust26.json').read_text(encoding='utf-8'))
    rows = []
    for x in para['answerable']:
        rows.append({**x, 'set': 'para'})
    for x in para['off_topic']:
        rows.append({'id': x['id'], 'q': x['q'], 'split': x['split'], 'set': 'para', 'kind': x['kind']})
    for i, x in enumerate(rust['answerable']):
        rows.append({'id': 'R%02d' % (i + 1), 'q': x['q'], 'want': x['want'], 'split': 'rust', 'set': 'rust', 'lang': 'en'})
    for i, q in enumerate(rust['off_topic']):
        rows.append({'id': 'RO%02d' % (i + 1), 'q': q, 'split': 'rust', 'set': 'rust', 'kind': 'pure'})
    return rows


def ask_all(sess, rows):
    out = {}
    for r in rows:
        st, _, body = sess.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote(r['q']))
        if st == 422:       # the empty question: the API refuses it, the experiment counted it as refused
            out[r['id']] = {'answered': False, 'sources': [], 'mode': 'refused-by-api', 'status': 422}
        else:
            out[r['id']] = {'answered': bool(body.get('answered')), 'sources': [x['source'] for x in body.get('results', [])], 'mode': body.get('mode'), 'status': st,
                            'fallback': body.get('fallback'), 'caveat': body.get('caveat'), 'top_score': (body.get('results') or [{}])[0].get('score')}
    return out


def evaluate(rows, got):
    ans = [r for r in rows if 'want' in r]
    off = [r for r in rows if 'want' not in r]
    m = {}
    if ans:
        for k in (1, 3):
            m['hit@%d' % k] = sum(got[r['id']]['answered'] and any(s in r['want'] for s in got[r['id']]['sources'][:k]) for r in ans) / len(ans)
        m['false_refusal'] = sum(not got[r['id']]['answered'] for r in ans) / len(ans)
        m['n_ans'] = len(ans)
    if off:
        m['off_refused'] = sum(not got[r['id']]['answered'] for r in off) / len(off)
        m['n_off'] = len(off)
    return m


def main():
    rows = load_rows()
    ref = json.loads((EXP / 'results.json').read_text(encoding='utf-8'))['systems']['dense_minilm_multi']
    bm = json.loads((EXP / 'results.json').read_text(encoding='utf-8'))['systems']['bm25']
    para_test = [r for r in rows if r['set'] == 'para' and r['split'] == 'test']
    slices = {
        'en test (all)': [r for r in para_test if r.get('lang') == 'en' or 'want' not in r],
        'fr+ar test': [r for r in para_test if r.get('lang') in ('fr', 'ar')],
        'original 26 + 5 off-topic': [r for r in rows if r['set'] == 'rust'],
        'off-topic test (pure)': [r for r in para_test if 'want' not in r and r['kind'] == 'pure'],
        'off-topic test (near-domain)': [r for r in para_test if 'want' not in r and r['kind'] == 'near-domain'],
    }
    token = secrets.token_hex(16)
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_rag_parity'
    data.mkdir(parents=True, exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    env_s = dict(os.environ, EMBED_TOKEN=token, EMBED_PORT=str(EPORT), EMBED_DEVICE='cpu', HF_HUB_OFFLINE='1', PYTHONIOENCODING='utf-8')
    side = subprocess.Popen([sys.executable, str(ROOT / 'deploy' / 'embed' / 'service.py')], env=env_s, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    srv = None
    try:
        check('the embedding sidecar starts (CPU, offline)', wait('http://127.0.0.1:%d/health' % EPORT, {'Authorization': 'Bearer ' + token}))
        try:
            urllib.request.urlopen('http://127.0.0.1:%d/health' % EPORT, timeout=3)
            check('the sidecar refuses a request without the token', False)
        except urllib.error.HTTPError as e:
            check('the sidecar refuses a request without the token', e.code == 401, e.code)
        env_c = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), EMBEDDER_URL='http://127.0.0.1:%d' % EPORT, EMBEDDER_TOKEN=token)
        srv = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env_c, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        check('the demo server starts', wait('http://127.0.0.1:%d/healthz' % PORT))
        acc = json.loads((data / 'demo_accounts.json').read_text())
        rev = pentest.Session('CG-2002', acc['CG-2002'])

        t0 = time.time()
        got = ask_all(rev, rows)
        secs = time.time() - t0
        modes = {v['mode'] for v in got.values()}
        check('every answer says it is semantic (or the API refused the empty question)', modes <= {'semantic', 'refused-by-api'}, modes)
        check('no answer fell back to keywords', not any(v.get('fallback') for v in got.values()), [k for k, v in got.items() if v.get('fallback')][:5])
        check('every semantic answer carries the caveat', all(v['caveat'] for v in got.values() if v['mode'] == 'semantic'))

        report = {'system': 'dense_minilm_multi through the product (CPU sidecar + Rust cosine), threshold 0.29', 'slices': {}, 'seconds_for_all_questions': round(secs, 1), 'questions': len(rows)}
        print('\nslice                         product            experiment (GPU)    (hit@3 / false refusal / off-topic refused)')
        for name, rs in slices.items():
            m = evaluate(rs, got)
            e = ref['slices'][name]
            report['slices'][name] = {'product': m, 'experiment': e}
            fmt = lambda x: ' '.join('%s=%.3f' % (k, v) for k, v in x.items() if k in ('hit@3', 'false_refusal', 'off_refused'))   # noqa: E731
            print('%-28s  %-44s %s' % (name, fmt(m), fmt(e)))
            for key in ('hit@1', 'hit@3', 'false_refusal', 'off_refused'):
                if key in e:
                    check('%s: %s equals the experiment (%.4f)' % (name, key, e[key]), abs(m.get(key, -1) - e[key]) < TOL, (m.get(key), e[key]))

        # the sidecar goes away: the server must say so and still answer from the keyword search
        side.kill()
        side.wait()
        time.sleep(1)
        st, _, body = rev.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote('highest unit price for SVC-LAB'))
        check('with the sidecar down the answer is the keyword search, labelled', st == 200 and body.get('mode') == 'keyword' and 'keyword search' in (body.get('fallback') or '') and body.get('answered'), (st, str(body)[:300]))
        orig = [r for r in rows if r['set'] == 'rust']
        kw = ask_all(rev, orig)
        m = evaluate(orig, kw)
        e = bm['slices']['original 26 + 5 off-topic']
        report['keyword_fallback_original_26'] = {'product': m, 'experiment_bm25': e}
        check('the keyword fallback keeps the old numbers on the original 26 (hit@3 %.3f)' % e['hit@3'], abs(m['hit@3'] - e['hit@3']) < TOL and abs(m['hit@1'] - e['hit@1']) < TOL, (m, e))
        check('every fallback answer says so', all(v['mode'] == 'keyword' for v in kw.values() if v['status'] == 200))

        report['per_question'] = {r['id']: {'q': r['q'], **got[r['id']]} for r in rows}
        out = ROOT / 'outputs' / 'defense' / 'rag_parity.json'
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=1, ensure_ascii=False), encoding='utf-8')
        print('\nwrote', out)
    finally:
        for p in (srv, side):
            if p and p.poll() is None:
                p.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
