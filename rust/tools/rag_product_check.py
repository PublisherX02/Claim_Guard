#!/usr/bin/env python3
"""The PRODUCT against the experiment for "Ask the rulebook" with bge-m3, phrases and out-of-scope topics (docs/55).

    HF_HOME=D:/hf_cache python rust/tools/rag_product_check.py

Starts the embedding sidecar (deploy/embed/service.py: BAAI/bge-m3 on CPU, offline) and a demo server whose EMBEDDER_URL points at it, sends the frozen v3 test
questions (questions_v3_test.json, written before the features) and the original 26 through the real route /api/v1/knowledge/search, and writes
outputs/defense/rag_product_check.json. The experiment ran on a GPU in fp16 and this runs on a CPU in fp32 through the Rust cosine, so the numbers are
compared within a tolerance, not for equality. Needs rust/target/release/cg.exe and the model in the Hugging Face cache named by HF_HOME.
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
EXP = ROOT / 'experiments' / 'rag_v2'
DEVICE = os.environ.get('CHECK_DEVICE', 'cpu')     # cpu (the original check) or cuda: the sidecar's EMBED_DEVICE
PORT, EPORT = 8499, 8099
pentest.PORT = PORT
pentest.raw.__defaults__ = (180, True)   # the first question embeds every passage on a CPU: that takes a while
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:400]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def wait(url, headers=None, tries=480):
    for _ in range(tries):
        try:
            urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    return False


def main():
    v3 = json.loads((EXP / 'questions_v3_test.json').read_text(encoding='utf-8'))
    rows = [dict(x, kind='answerable') for x in v3['answerable']] + v3['off_topic']
    exp = json.loads((EXP / 'results_v3.json').read_text(encoding='utf-8'))['bge_m3+phrases+decoys']
    token = secrets.token_hex(16)
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_rag_product'
    data.mkdir(parents=True, exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    env_s = dict(os.environ, EMBED_TOKEN=token, EMBED_PORT=str(EPORT), EMBED_DEVICE=DEVICE, HF_HUB_OFFLINE='1', PYTHONIOENCODING='utf-8')
    side = subprocess.Popen([sys.executable, str(ROOT / 'deploy' / 'embed' / 'service.py')], env=env_s, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    srv = None
    try:
        check('the embedding sidecar starts (bge-m3, %s, offline)' % DEVICE, wait('http://127.0.0.1:%d/health' % EPORT, {'Authorization': 'Bearer ' + token}))
        req = urllib.request.Request('http://127.0.0.1:%d/health' % EPORT, headers={'Authorization': 'Bearer ' + token})
        h = json.loads(urllib.request.urlopen(req, timeout=10).read())
        check('the sidecar reports bge-m3 with 1024 dimensions', h.get('model') == 'BAAI/bge-m3' and h.get('dim') == 1024, h)
        env_c = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), EMBEDDER_URL='http://127.0.0.1:%d' % EPORT, EMBEDDER_TOKEN=token, EMBEDDER_TIMEOUT='60')
        srv = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env_c, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        check('the demo server starts', wait('http://127.0.0.1:%d/healthz' % PORT))
        acc = json.loads((data / 'demo_accounts.json').read_text())
        rev = pentest.Session('CG-2002', acc['CG-2002'])
        got, lat = {}, []
        for r in rows:
            t0 = time.time()
            st, _, b = rev.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote(r['q']))
            lat.append(time.time() - t0)
            got[r['id']] = {'status': st, 'answered': bool(b.get('answered')), 'sources': [x['source'] for x in b.get('results', [])], 'mode': b.get('mode'), 'out_of_scope': b.get('out_of_scope'), 'fallback': b.get('fallback')}
        check('every answer is semantic, none fell back to keywords', all(v['mode'] == 'semantic' and not v['fallback'] for v in got.values()), {k: v['mode'] for k, v in got.items() if v['mode'] != 'semantic'})
        ans = [r for r in rows if 'want' in r]
        near = [r for r in rows if 'want' not in r and r['kind'] == 'near-domain']
        pure = [r for r in rows if 'want' not in r and r['kind'] == 'pure']
        hit3 = sum(got[r['id']]['answered'] and any(s in r['want'] for s in got[r['id']]['sources'][:3]) for r in ans) / len(ans)
        hit1 = sum(got[r['id']]['answered'] and any(s in r['want'] for s in got[r['id']]['sources'][:1]) for r in ans) / len(ans)
        near_ref = sum(not got[r['id']]['answered'] for r in near) / len(near)
        pure_ref = sum(not got[r['id']]['answered'] for r in pure) / len(pure)
        named = sum(bool(got[r['id']]['out_of_scope']) for r in near + pure)
        e = exp['slices']
        print('\n                       product   experiment (GPU fp16)')
        print('answerable hit@3       %.2f      %.2f' % (hit3, e['answerable (20)']['hit3']))
        print('answerable hit@1       %.2f      %.2f' % (hit1, e['answerable (20)']['hit1']))
        print('near-domain refused    %.2f      %.2f' % (near_ref, e['near-domain refused (20)']['refused']))
        print('pure off-topic refused %.2f      %.2f' % (pure_ref, e['pure refused (4)']['refused']))
        check('hit@3 is within 0.10 of the experiment', abs(hit3 - e['answerable (20)']['hit3']) <= 0.10, (hit3, e['answerable (20)']['hit3']))
        check('near-domain refusal is within 0.10 of the experiment', abs(near_ref - e['near-domain refused (20)']['refused']) <= 0.10, (near_ref, e['near-domain refused (20)']['refused']))
        check('pure off-topic questions are all refused', pure_ref == 1.0, pure_ref)
        check('a refusal names the out-of-scope topic when it knows it', named >= 5, named)
        check('a refused question carries no passages', all(not v['sources'] for v in got.values() if not v['answered']))
        try:
            rss = subprocess.run(['powershell', '-NoProfile', '-Command', '(Get-Process -Id %d).WorkingSet64 / 1MB' % side.pid], capture_output=True, text=True, timeout=20).stdout.strip()
            rss_mb = round(float(rss))
        except Exception:  # noqa: BLE001
            rss_mb = None
        print('sidecar resident memory: %s MB' % rss_mb)
        side.kill()
        side.wait()
        time.sleep(1)
        st, _, body = rev.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote('highest unit price for SVC-LAB'))
        check('with the sidecar down the answer is the labelled keyword search', st == 200 and body.get('mode') == 'keyword' and body.get('answered'), (st, str(body)[:200]))
        report = {'model': 'BAAI/bge-m3 (CPU fp32 sidecar + Rust cosine), threshold 0.485', 'product': {'hit3': hit3, 'hit1': hit1, 'near_refused': near_ref, 'pure_refused': pure_ref, 'refusals_naming_a_topic': named},
                  'experiment': {'hit3': e['answerable (20)']['hit3'], 'near_refused': e['near-domain refused (20)']['refused']}, 'device': DEVICE, 'first_question_seconds': round(lat[0], 3), 'seconds_per_question_p50': round(sorted(lat)[len(lat) // 2], 3), 'seconds_per_question_max': round(max(lat), 3), 'sidecar_resident_mb': rss_mb,
                  'per_question': {r['id']: {'q': r['q'], **got[r['id']]} for r in rows}}
        out = ROOT / 'outputs' / 'defense' / ('rag_product_check.json' if DEVICE == 'cpu' else 'rag_product_check_%s.json' % DEVICE)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=1, ensure_ascii=False), encoding='utf-8')
        print('\nlatency per question: first %.2f s, p50 %.2f s, max %.2f s; wrote %s' % (report['first_question_seconds'], report['seconds_per_question_p50'], report['seconds_per_question_max'], out))
    finally:
        for p in (srv, side):
            if p and p.poll() is None:
                p.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
