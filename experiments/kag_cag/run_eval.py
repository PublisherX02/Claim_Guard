"""Measures the three tiers of "Ask the rulebook" (docs/60) on this machine's GPU: passage search alone, the knowledge graph, the answer cache, and Gemma 3 4B
(Ollama) choosing a query and writing answers up.

    python experiments/kag_cag/run_eval.py            writes outputs/defense/kag_cag_eval.json

Needs: target/release/cg.exe, Ollama with the model gemma3-ask:4b (ollama create gemma3-ask:4b -f experiments/kag_cag/Modelfile: gemma3:4b with an 8192-token
context, because Ollama's default of 4096 would silently cut the rulebook prefix), and the bge-m3 weights in D:/hf_cache. The embedding sidecar is started here on the GPU.
Phases (each is one `cg ask --file questions.jsonl`):
  A  passage search only (the system before this work)             B  graph + passage search, answer cache (round 2)
  C  B plus the model choosing a query when the planner cannot     D  C plus the model writing answers, facts only
  E  D with the rulebook as the front of every prompt (CAG prefix)
Score: a question is answered when EVERY gold token (rule id, value, policy id) is in the answer text. For tier `graph` the text is the cited facts; for
`vector` the four quoted passages and their sources; "written" is the model's text, scored separately and shown only if the check accepted it.
"""
import json, os, statistics, subprocess, sys, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CG = ROOT / 'rust/target/release/cg.exe'
OUT = ROOT / 'outputs/defense/kag_cag_eval.json'
Q = json.loads((ROOT / 'experiments/kag_cag/questions.json').read_text(encoding='utf8'))
TOKEN, PORT = 'kagcag-token', 8097


def env_base(**extra):
    e = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), PYTHONIOENCODING='utf-8')
    for k in ('ASK_LLM_PROVIDER', 'ASK_LLM_PREFIX', 'EMBEDDER_URL'):
        e.pop(k, None)
    e.update(extra)
    return e


def cg_ask(file, flags, env, timeout=3600):
    t = time.time()
    p = subprocess.run([str(CG), 'ask', '--file', str(file)] + flags, capture_output=True, text=True, encoding='utf-8', env=env, timeout=timeout)
    rows = [json.loads(l) for l in p.stdout.splitlines() if l.startswith('{')]
    if not rows:
        raise SystemExit('cg ask failed: ' + p.stdout[-300:] + p.stderr[-300:])
    return rows, time.time() - t


def text_of(r):
    if r.get('tier') == 'graph':
        return r.get('answer', '')
    return ' '.join(f"{x['text']} {x['source']}" for x in r.get('results', []))


def ok(r, gold):
    t = text_of(r).lower()
    return all(g.lower() in t for g in gold)


def written_ok(r, gold):
    return bool(r.get('written')) and all(g.lower() in r['written'].lower() for g in gold)


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))] if xs else None


def summarize(rows, rnd=1):
    gold = {q['id']: q for q in Q}
    rows = [r for r in rows if r.get('round', 1) == rnd]
    by = {}
    for r in rows:
        q = gold[r['id']]
        d = by.setdefault(q['cat'], {'n': 0, 'answered': 0, 'graph': 0, 'written_shown': 0, 'written_complete': 0, 'written_refused': 0})
        d['n'] += 1
        d['answered'] += ok(r, q['gold'])
        d['graph'] += r.get('tier') == 'graph'
        d['written_shown'] += bool(r.get('written'))
        d['written_complete'] += written_ok(r, q['gold'])
        d['written_refused'] += bool(r.get('written_refused'))
    n = len(rows)
    tot = {k: sum(d[k] for d in by.values()) for k in ('answered', 'graph', 'written_shown', 'written_complete', 'written_refused')}
    lat = {}
    for r in rows:
        lat.setdefault(r.get('tier', '?') + ('+cached' if r.get('cached') else ''), []).append(r['wall_ms'])
    return {'n': n, **tot, 'by_category': by, 'latency_ms': {k: {'n': len(v), 'p50': pct(v, .5), 'p95': pct(v, .95), 'max': max(v)} for k, v in lat.items()}}


def start_sidecar():
    env = dict(os.environ, EMBED_TOKEN=TOKEN, EMBED_PORT=str(PORT), EMBED_DEVICE='cuda', HF_HUB_OFFLINE='1', HF_HOME='D:/hf_cache', PYTHONIOENCODING='utf-8')
    p = subprocess.Popen([sys.executable, str(ROOT / 'deploy/embed/service.py')], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    t0 = time.time()
    while True:
        try:
            urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{PORT}/health', headers={'Authorization': 'Bearer ' + TOKEN}), timeout=3)
            break
        except Exception:
            time.sleep(1)
            if time.time() - t0 > 600:
                p.kill()
                raise SystemExit('the embedding sidecar did not start')
    return p, time.time() - t0


def ollama(path, body):
    req = urllib.request.Request('http://127.0.0.1:11434' + path, data=json.dumps(body).encode(), headers={'content-type': 'application/json'})
    return json.loads(urllib.request.urlopen(req, timeout=600).read())


def main():
    res = {'machine': 'RTX 4060 Laptop GPU 8 GB', 'model': 'gemma3:4b (num_ctx 8192) via Ollama', 'questions': len(Q)}
    side, load_s = start_sidecar()
    res['embedder_load_s'] = round(load_s, 1)
    emb = dict(EMBEDDER_URL=f'http://127.0.0.1:{PORT}', EMBEDDER_TOKEN=TOKEN, EMBEDDER_TIMEOUT='30')
    llm = dict(ASK_LLM_PROVIDER='ollama', OLLAMA_MODEL='gemma3-ask:4b', ASK_LLM_TIMEOUT='180')
    qf = ROOT / 'experiments/kag_cag/questions.jsonl'
    try:
        # warm the embedder's passage cache is part of the first question; do one throwaway run so every phase starts equal
        cg_ask(qf, ['--vector-only'], env_base(**emb))
        print('A: passage search only', flush=True)
        rows, _ = cg_ask(qf, ['--vector-only'], env_base(**emb))
        res['A_vector_only'] = summarize(rows)
        print('B: graph + passage search, round 2 = answer cache', flush=True)
        rows, _ = cg_ask(qf, ['--repeat', '2'], env_base(**emb))
        res['B_graph_router'] = summarize(rows, 1)
        res['B_round2_cache'] = summarize(rows, 2)
        print('C: + model chooses the query', flush=True)
        ollama('/api/generate', {'model': 'gemma3-ask:4b', 'prompt': 'hi', 'stream': False, 'keep_alive': '30m'})
        rows, _ = cg_ask(qf, [], env_base(**emb, **llm))
        res['C_model_planner'] = summarize(rows)
        res['C_rows'] = [{k: r.get(k) for k in ('id', 'tier', 'planner', 'wall_ms', 'understood_as')} for r in rows]
        print('D: + model writes answers (facts only)', flush=True)
        rows, _ = cg_ask(qf, ['--generate'], env_base(**emb, **llm))
        res['D_generate'] = summarize(rows)
        res['D_refusals'] = [{'id': r['id'], 'why': r['written_refused']} for r in rows if r.get('written_refused')]
        res['D_rows'] = [{k: r.get(k) for k in ('id', 'tier', 'wall_ms', 'written', 'written_refused')} for r in rows]
        print('E: + the rulebook as the prompt prefix (CAG)', flush=True)
        rows, _ = cg_ask(qf, ['--generate'], env_base(**emb, **llm, ASK_LLM_PREFIX='1'))
        res['E_generate_with_prefix'] = summarize(rows)
        res['E_refusals'] = [{'id': r['id'], 'why': r['written_refused']} for r in rows if r.get('written_refused')]
        res['E_rows'] = [{k: r.get(k) for k in ('id', 'tier', 'wall_ms', 'written', 'written_refused')} for r in rows]
        # the prefix itself: size, and cold versus warm on the model
        ctx = subprocess.run([str(CG), 'ask', '--context'], capture_output=True, text=True, encoding='utf-8', env=env_base()).stdout
        probe = ctx + '\n\nQuestion: which rule checks the network?\nAnswer in one sentence:'
        first = ollama('/api/generate', {'model': 'gemma3-ask:4b', 'prompt': probe + ' (1)', 'stream': False, 'options': {'num_predict': 24}})
        second = ollama('/api/generate', {'model': 'gemma3-ask:4b', 'prompt': probe + ' (2)', 'stream': False, 'options': {'num_predict': 24}})
        res['prefix_probe'] = {'prompt_tokens': first.get('prompt_eval_count'), 'first_prompt_eval_ms': round(first.get('prompt_eval_duration', 0) / 1e6), 'second_prompt_eval_ms': round(second.get('prompt_eval_duration', 0) / 1e6),
                               'second_prompt_tokens_computed': second.get('prompt_eval_count')}
    finally:
        side.kill()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(res, ensure_ascii=False, indent=1), encoding='utf-8')
    for k in ('A_vector_only', 'B_graph_router', 'B_round2_cache', 'C_model_planner', 'D_generate', 'E_generate_with_prefix'):
        s = res[k]
        print(f"{k:28s} answered {s['answered']}/{s['n']}  graph {s['graph']}  written_shown {s['written_shown']}  written_complete {s['written_complete']}  refused {s['written_refused']}")
    print(json.dumps(res.get('prefix_probe')))


if __name__ == '__main__':
    main()
