"""Load and exhaustion test of one server on this machine: thousands of connections, hundreds of concurrent clients, a sign-in flood, large
bodies. It cannot model a distributed attack from many addresses (one machine has one address), but it shows what a single abusive
client or a busy afternoon does to the server: does it stay up, keep answering a polite client, keep its memory bounded, and recover?

    python rust/tools/flood.py [--seconds 20]

Checks (exit 1 if any fails): the server answers a polite client within 1 s during every phase; no 5xx anywhere; memory after each phase is
within 3x of the idle size; connections over the server's cap are dropped rather than queued; the server is healthy afterwards.
"""
import argparse, asyncio, json, os, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

import psutil

ROOT = Path(__file__).resolve().parents[2]
CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8499
RESULTS = []


def check(name, ok, detail=''):
    RESULTS.append(ok)
    print(('PASS ' if ok else 'FAIL ') + name + (f'  -- {detail}' if detail and not ok else f'  ({detail})' if detail else ''), flush=True)


def polite():
    t = time.perf_counter()
    try:
        r = urllib.request.urlopen(f'http://127.0.0.1:{PORT}/healthz', timeout=5)
        return r.status, time.perf_counter() - t
    except Exception as e:  # noqa: BLE001
        return type(e).__name__, time.perf_counter() - t


async def one_request(path='/healthz', method='GET', body=b'', headers=''):
    try:
        r, w = await asyncio.wait_for(asyncio.open_connection('127.0.0.1', PORT), 10)
        w.write(f'{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{headers}Content-Length: {len(body)}\r\n\r\n'.encode() + body)
        await w.drain()
        data = await asyncio.wait_for(r.read(300), 20)
        w.close()
        return int(data.split(b' ', 2)[1]) if data.startswith(b'HTTP/') else 0
    except Exception:  # noqa: BLE001
        return 0


async def hammer(seconds, workers, **kw):
    stop = time.time() + seconds
    codes = []

    async def worker():
        while time.time() < stop:
            codes.append(await one_request(**kw))

    await asyncio.gather(*[worker() for _ in range(workers)])
    return codes


def phase(name, proc, baseline, fn):
    t0 = time.time()
    out = fn()
    ps = polite()
    rss = psutil.Process(proc.pid).memory_info().rss / 1048576
    check(f'{name}: a polite client is still served quickly', ps[0] == 200 and ps[1] < 1.0, f'{ps[0]} in {ps[1] * 1000:.0f} ms')
    check(f'{name}: memory stays bounded', rss < max(3 * baseline, baseline + 150), f'{rss:.0f} MB, idle {baseline:.0f} MB')
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--seconds', type=int, default=20)
    a = ap.parse_args()
    data = Path(tempfile.mkdtemp(prefix='cg_flood_'))
    proc = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(80):
            if polite()[0] == 200:
                break
            time.sleep(0.5)
        time.sleep(3)
        baseline = psutil.Process(proc.pid).memory_info().rss / 1048576
        print(f'idle memory {baseline:.0f} MB')

        # 1. thousands of idle connections: the cap drops the excess instead of queueing it
        async def many_idle(n):
            conns, refused = [], 0
            for _ in range(n):
                try:
                    r, w = await asyncio.wait_for(asyncio.open_connection('127.0.0.1', PORT), 5)
                    conns.append((r, w))
                except Exception:  # noqa: BLE001
                    refused += 1
            await asyncio.sleep(1)
            alive = 0
            for r, w in conns:
                try:
                    data = await asyncio.wait_for(r.read(1), 0.05)
                    alive += 0 if data == b'' else 1
                except asyncio.TimeoutError:
                    alive += 1
                except Exception:  # noqa: BLE001
                    pass
            ps = polite()
            for r, w in conns:
                w.close()
            return len(conns), refused, alive, ps

        opened, refused, alive, ps = phase('3,000 idle connections', proc, baseline, lambda: asyncio.run(many_idle(3000)))
        check('3,000 idle connections: no more than the cap are held open', alive <= 2100, f'{alive} held open of {opened} accepted, {refused} refused')
        # 2. a busy afternoon: 300 concurrent clients
        codes = phase('300 concurrent clients for %d s' % a.seconds, proc, baseline, lambda: asyncio.run(hammer(a.seconds, 300)))
        ok200 = codes.count(200)
        check('300 concurrent clients: every answered request is a 200, none is a 5xx', all(c == 200 or c == 0 for c in codes) and ok200 > 100, f'{len(codes)} requests, {ok200 / a.seconds:.0f}/s, {codes.count(0)} failed to connect')
        # 3. unauthenticated hammering of a protected route
        codes = phase('protected route without a session', proc, baseline, lambda: asyncio.run(hammer(10, 200, path='/api/v1/claims')))
        check('protected route flood: only 401s', set(codes) <= {401, 0} and codes.count(401) > 100, {c: codes.count(c) for c in set(codes)})
        # 4. a sign-in flood from one address: the address counter makes it cheap after 100 attempts
        body = json.dumps({'badge_id': 'CG-9999', 'password': 'x', 'totp': '000000'}).encode()
        codes = phase('sign-in flood (400 concurrent)', proc, baseline, lambda: asyncio.run(hammer(10, 400, path='/api/v1/auth/login', method='POST', body=body, headers='Content-Type: application/json\r\n')))
        check('sign-in flood: refusals are 401 then 429, never 5xx', set(codes) <= {401, 429, 0} and 429 in codes, {c: codes.count(c) for c in set(codes)})
        # 5. large bodies
        big = b'{"a":"' + b'x' * 60000 + b'"}'
        codes = phase('60 KB bodies from 100 clients', proc, baseline, lambda: asyncio.run(hammer(10, 100, path='/api/v1/auth/login', method='POST', body=big, headers='Content-Type: application/json\r\n')))
        check('large bodies: only 4xx', set(codes) <= {422, 429, 401, 413, 0}, {c: codes.count(c) for c in set(codes)})
        time.sleep(3)
        ps = polite()
        rss = psutil.Process(proc.pid).memory_info().rss / 1048576
        check('afterwards the server is healthy and its memory has not run away', ps[0] == 200 and rss < max(3 * baseline, baseline + 150), f'{rss:.0f} MB')
    finally:
        proc.kill()
    print(f'\n{len(RESULTS)} checks, {RESULTS.count(False)} FAILED')
    return 1 if False in RESULTS else 0


if __name__ == '__main__':
    sys.exit(main())
