import json, sys
sys.path.insert(0, '.')
from bm25_port import load_corpus, BM25
ps = load_corpus(); ix = BM25(ps)
d = json.load(open('questions_rust26.json'))
a1 = a3 = 0; miss = []
for x in d['answerable']:
    h = ix.search(x['q'], 3)
    a1 += bool(h) and h[0]['source'] in x['want']
    ok = any(y['source'] in x['want'] for y in h); a3 += ok
    if not ok: miss.append(x['q'])
n = len(d['answerable'])
print('passages', len(ps), 'hit@1 %.0f%% hit@3 %.0f%%' % (100*a1/n, 100*a3/n), 'misses', miss)
print('off-topic refused', sum(1 for q in d['off_topic'] if not ix.search(q, 3)), '/', len(d['off_topic']))
