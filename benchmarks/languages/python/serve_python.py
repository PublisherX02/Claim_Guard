"""HTTP control: the same POST /evaluate contract on FastAPI + uvicorn (the stack the real API uses).

    python serve_python.py -corpus ../corpus -serve 127.0.0.1:9105 [-threads N]   # N uvicorn worker processes
"""
import argparse, json, os, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tests'))
import oracle  # noqa: E402
from fastapi import FastAPI, Request, Response  # noqa: E402

CODE = {oracle.PASS: 'P', oracle.FAIL: 'F', oracle.UNABLE: 'U', oracle.NA: 'N'}
corpus = os.environ.get('BENCH_CORPUS', '')
PACK = {n: json.loads((Path(corpus) / 'pack' / f'{n}.json').read_text(encoding='utf-8')) for n in ('policies', 'services')} if corpus else {}
app = FastAPI()


@app.get('/healthz')
def healthz():
    return Response('ok')


@app.post('/evaluate')
async def evaluate(request: Request):
    body = await request.body()
    if len(body) > (1 << 20):
        return Response(status_code=413)
    try:
        c = json.loads(body)
        r = oracle.evaluate(c, PACK)
        return Response(json.dumps({'claim_id': c['claim_id'], 'statuses': ''.join(CODE[r[k]] for k in sorted(r))}, separators=(',', ':')), media_type='application/json')
    except Exception:
        return Response(status_code=400)


if __name__ == '__main__':
    import uvicorn
    ap = argparse.ArgumentParser(); ap.add_argument('-corpus', required=True); ap.add_argument('-serve', required=True); ap.add_argument('-threads', type=int, default=1)
    a = ap.parse_args()
    os.environ['BENCH_CORPUS'] = a.corpus
    host, port = a.serve.split(':')
    uvicorn.run('serve_python:app', host=host, port=int(port), workers=a.threads, log_level='error', access_log=False)
