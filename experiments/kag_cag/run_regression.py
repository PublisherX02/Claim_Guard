"""Does adding the knowledge graph in front of passage search change answers it should not? (docs/60)

Runs the held-out sets of the earlier retrieval experiment (experiments/rag_v2/questions_v3_test.json: 20 answerable questions, 24 off-topic ones, in English,
French and Arabic) through passage search alone and through the router, and reports:
  * answerable questions the graph took over (tier graph) and whether its facts still cite the wanted source, and
  * off-topic questions the graph took over (it must be zero: the graph must never answer what the rulebook does not cover).

    python experiments/kag_cag/run_regression.py        writes outputs/defense/kag_cag_regression.json
"""
import json, os, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CG = ROOT / 'rust/target/release/cg.exe'
TOKEN, PORT = 'kagcag-token', 8097
sets = json.loads((ROOT / 'experiments/rag_v2/questions_v3_test.json').read_text(encoding='utf8'))
ans, off = sets['answerable'], sets['off_topic']
tmp = Path(tempfile.mkdtemp()) / 'q.jsonl'
tmp.write_text('\n'.join(json.dumps({'id': x['id'], 'q': x['q']}, ensure_ascii=False) for x in ans + off) + '\n', encoding='utf8')

env = dict(os.environ, EMBED_TOKEN=TOKEN, EMBED_PORT=str(PORT), EMBED_DEVICE='cuda', HF_HUB_OFFLINE='1', HF_HOME='D:/hf_cache', PYTHONIOENCODING='utf-8')
side = subprocess.Popen([sys.executable, str(ROOT / 'deploy/embed/service.py')], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    while True:
        try:
            urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{PORT}/health', headers={'Authorization': 'Bearer ' + TOKEN}), timeout=3)
            break
        except Exception:
            time.sleep(1)
    e2 = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), EMBEDDER_URL=f'http://127.0.0.1:{PORT}', EMBEDDER_TOKEN=TOKEN, EMBEDDER_TIMEOUT='30', PYTHONIOENCODING='utf-8')

    def run(flags):
        p = subprocess.run([str(CG), 'ask', '--file', str(tmp)] + flags, capture_output=True, text=True, encoding='utf-8', env=e2)
        return {r['id']: r for r in (json.loads(l) for l in p.stdout.splitlines() if l.startswith('{'))}

    run(['--vector-only'])  # warm
    base, routed = run(['--vector-only']), run([])
finally:
    side.kill()

out = {'answerable': {'n': len(ans), 'taken_by_graph': [], 'top3_changed': []}, 'off_topic': {'n': len(off), 'taken_by_graph': [], 'answered_before': 0, 'answered_after': 0}}
for x in ans:
    b, r = base[x['id']], routed[x['id']]
    top = lambda d: [h['source'] for h in d.get('results', [])][:3]
    if r['tier'] == 'graph':
        cited = any(w.replace('rule ', '') in r['answer'] or w in r['answer'] for w in x['want'])
        out['answerable']['taken_by_graph'].append({'id': x['id'], 'q': x['q'], 'wanted': x['want'], 'facts_cite_wanted': cited, 'understood_as': r.get('understood_as')})
    elif top(b) != top(r):
        out['answerable']['top3_changed'].append(x['id'])
for x in off:
    b, r = base[x['id']], routed[x['id']]
    out['off_topic']['answered_before'] += bool(b.get('answered'))
    out['off_topic']['answered_after'] += bool(r.get('answered'))
    if r['tier'] == 'graph':
        out['off_topic']['taken_by_graph'].append({'id': x['id'], 'q': x['q'], 'understood_as': r.get('understood_as')})
(ROOT / 'outputs/defense/kag_cag_regression.json').write_text(json.dumps(out, ensure_ascii=False, indent=1), encoding='utf-8')
print(json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in out['answerable'].items()}), json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in out['off_topic'].items()}))
print('answerable taken by graph:', json.dumps(out['answerable']['taken_by_graph'], ensure_ascii=False)[:1500])
print('off-topic taken by graph:', json.dumps(out['off_topic']['taken_by_graph'], ensure_ascii=False)[:1500])
