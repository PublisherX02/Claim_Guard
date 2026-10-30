#!/usr/bin/env python3
"""S3. "Ask the rulebook" by meaning on the server: does the embedding container reproduce the quality measured in docs/52, and how fast is it (docs/65)?

    CG_URL=... CG_VIEW_BADGE=... python experiments/server/s03_search.py --run <name> [--levels 1,2,4,8] [--repeat 3]

Quality: the 26 frozen questions and 5 off-topic strings of experiments/rag_semantic/questions_rust26.json go through GET /api/v1/knowledge/search.
A question is a hit when the expected rule appears among the cited facts of the answer; an off-topic string is correct when the system refuses to
answer. Speed: the same questions at rising concurrency, latency percentiles and questions per second.
The route limits each person to ASK_PER_MINUTE questions a minute. For this experiment set ASK_PER_MINUTE=100000 in .env and restart the api
(docs/65 section 3); put it back afterwards. If the limit still bites, the 429 answers are counted and reported, not hidden.
"""
import argparse
import json
import sys
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import ROOT, Api, Run, summary  # noqa: E402

QUESTIONS = ROOT / 'experiments' / 'rag_semantic' / 'questions_rust26.json'


def ask(api, q):
    status, body, dt = api.call('GET', '/api/v1/knowledge/search?q=' + urllib.parse.quote(q))
    return status, body, dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--levels', default='1,2,4,8')
    ap.add_argument('--repeat', type=int, default=3)
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run, api = Run(a.run, a.dry), Api(prefix='CG_VIEW_').login()      # the rulebook needs claims.view, which the administrator does not have
    qs = json.loads(QUESTIONS.read_text(encoding='utf-8'))
    answerable, off = qs['answerable'], qs['off_topic']
    hits, misses, lat, limited, statuses = 0, [], [], 0, {}
    for item in answerable:
        status, body, dt = ask(api, item['q'])
        lat.append(dt)
        limited += status == 429
        statuses[str(status)] = statuses.get(str(status), 0) + 1
        if status == 403 and not statuses.get('200'):
            raise SystemExit('the viewer account may not read claims or ask the rulebook (HTTP 403): CG_VIEW_BADGE must be a level 1 viewer')
        text = json.dumps(body, ensure_ascii=False).lower() if status == 200 else ''
        if status == 200 and body.get('answered') and all(w.lower() in text for w in item['want']):
            hits += 1
        else:
            misses.append({'q': item['q'], 'want': item['want'], 'status': status, 'answered': body.get('answered') if isinstance(body, dict) else None})
    refused = 0
    for q in off:
        status, body, dt = ask(api, q)
        limited += status == 429
        refused += status == 200 and body.get('answered') is False
    speed = []
    flat = [i['q'] for i in answerable] * a.repeat
    for conc in [int(x) for x in a.levels.split(',')]:
        t0 = time.perf_counter()
        with ThreadPoolExecutor(conc) as pool:
            res = list(pool.map(lambda q: ask(api, q), flat))
        wall = time.perf_counter() - t0
        ok = [dt for s, _, dt in res if s == 200]
        limited += sum(1 for s, _, _ in res if s == 429)
        speed.append({'concurrency': conc, 'requests': len(res), 'ok': len(ok), 'questions_per_second': len(ok) / wall, 'latency_s': summary(ok)})
    result = {'quality': {'answerable': len(answerable), 'hits': hits, 'hit_rate': hits / len(answerable), 'off_topic': len(off), 'refused': refused,
                          'refusal_rate': refused / len(off), 'misses': misses, 'reference': 'docs/52 and docs/55: the numbers measured on the build PC'},
              'single_latency_s': summary(lat), 'speed': speed, 'rate_limited_429': limited, 'statuses_first_pass': statuses}
    result['checks'] = {'hit_rate_at_least_0_9': result['quality']['hit_rate'] >= 0.9, 'all_off_topic_refused': refused == len(off), 'not_rate_limited': limited == 0}
    run.write('s03_search', result)


if __name__ == '__main__':
    main()
