#!/usr/bin/env python3
"""What does a reverse proxy cost? The same server measured directly, behind nginx and behind Caddy (docs/54). LOCAL DEMO scope, one laptop.

    python rust/tools/proxy_load.py [--cg path/to/release/cg.exe] [--requests 3000] [--connections 16]

Three requests: /healthz (no work at all, so this is the proxy's own cost), /api/v1/work/inbox (a signed-in read from the store) and /ui/app.js (a static file, asked for
with gzip). Each runs over keep-alive connections. "direct" is plain HTTP to the server; both proxies speak TLS to the client, so their numbers include TLS and the
direct ones do not. nginx runs the deploy/nginx/nginx.conf with its rate-limit lines removed (a load test would only measure the limiter); Caddy runs deploy/Caddyfile.internal.
The client is one python process: its own speed is the ceiling, so read the numbers as a comparison between rows, not as the server's capacity.
Writes outputs/defense/proxy_load.json.
"""
import argparse
import http.client
import json
import os
import re
import ssl
import statistics
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pyotp  # noqa: E402
import nginx_proxy_test as npt  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CADDY = 'caddy:2@sha256:f2a1290d0463aad60660d4ec134943f183ee2a5f6c3eb7bf32dd984f2f020772'
CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE


def connect(target):
    if target == 'direct':
        return http.client.HTTPConnection('127.0.0.1', 8443, timeout=30)
    # Caddy serves only the name it was given (localhost) and needs it in the TLS handshake; nginx answers to any name
    return http.client.HTTPSConnection('localhost' if target == 'caddy' else '127.0.0.1', {'nginx': 9444, 'caddy': 9443}[target], context=CTX, timeout=30)


def worker(target, path, headers, n, lat, errors):
    c = connect(target)
    for _ in range(n):
        t0 = time.perf_counter()
        try:
            c.request('GET', path, headers=headers)
            r = c.getresponse()
            r.read()
            if r.status != 200:
                errors.append(r.status)
        except (OSError, http.client.HTTPException):
            errors.append('conn')
            c.close()
            c = connect(target)
            continue
        lat.append((time.perf_counter() - t0) * 1000)
    c.close()


def measure(target, path, headers, total, conns):
    worker(target, path, headers, 50, [], [])        # warm-up
    lat, errors, threads = [], [], []
    per = total // conns
    t0 = time.perf_counter()
    for _ in range(conns):
        t = threading.Thread(target=worker, args=(target, path, headers, per, lat, errors))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    took = time.perf_counter() - t0
    lat.sort()
    q = lambda p: lat[min(len(lat) - 1, int(len(lat) * p))] if lat else None  # noqa: E731
    return {'requests': per * conns, 'errors': len(errors), 'per_second': round(len(lat) / took), 'p50_ms': round(q(0.5) or 0, 2), 'p95_ms': round(q(0.95) or 0, 2), 'p99_ms': round(q(0.99) or 0, 2)}


def main():
    ap = argparse.ArgumentParser()
    rel = ROOT / 'rust' / 'target' / 'release' / 'cg.exe'
    ap.add_argument('--cg', default=str(rel))
    ap.add_argument('--requests', type=int, default=3000)
    ap.add_argument('--connections', type=int, default=16)
    a = ap.parse_args()
    work = Path(os.environ.get('TEMP', '/tmp')) / 'cg_proxy_load'
    npt.TLS = work / 'tls'
    npt.TLS.mkdir(parents=True, exist_ok=True)
    npt.make_certificate(npt.TLS)
    conf_text = re.sub(r'^\s*limit_req .*;\n', '', (ROOT / 'deploy' / 'nginx' / 'nginx.conf').read_text(encoding='utf-8'), flags=re.M)
    conf = work / 'nginx_nolimit.conf'
    conf.write_text(conf_text, encoding='utf-8')
    data = work / 'data'
    data.mkdir(exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    subprocess.run(['docker', 'rm', '-f', 'cg-load-caddy'], capture_output=True)
    server = subprocess.Popen([a.cg, 'serve', '--demo', '--queue', '--port', '8443', '--data-dir', str(data)], env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)),
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    results = {}
    try:
        for _ in range(120):
            try:
                urllib.request.urlopen('http://127.0.0.1:8443/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        c = http.client.HTTPConnection('127.0.0.1', 8443, timeout=10)
        c.request('POST', '/api/v1/auth/login', body=json.dumps({'badge_id': 'CG-2002', 'password': acc['CG-2002']['password'], 'totp': pyotp.TOTP(acc['CG-2002']['secret']).now()}),
                  headers={'Content-Type': 'application/json'})
        r = c.getresponse()
        r.read()
        cookie = 'cg_session=' + [v for k, v in r.getheaders() if k.lower() == 'set-cookie' and 'cg_session=' in v][0].split('cg_session=')[1].split(';')[0]
        c.close()
        npt.docker_run(str(conf))
        subprocess.run(['docker', 'run', '-d', '--name', 'cg-load-caddy', '-p', '127.0.0.1:9443:443', '--add-host', 'api:host-gateway', '-e', 'CG_SITE=localhost',
                        '-v', '%s:/etc/caddy/Caddyfile:ro' % (ROOT / 'deploy' / 'Caddyfile.internal'), CADDY], capture_output=True)
        for _ in range(60):      # Caddy makes its own certificate on first start: wait until it answers
            try:
                cc = connect('caddy')
                cc.request('GET', '/healthz')
                if cc.getresponse().status == 200:
                    break
            except (OSError, http.client.HTTPException):
                pass
            time.sleep(1)
        time.sleep(2)
        endpoints = [('healthz', '/healthz', {}), ('inbox (signed in)', '/api/v1/work/inbox', {'Cookie': cookie}), ('console file, gzip', '/ui/app.js', {'Accept-Encoding': 'gzip'})]
        for target in ('direct', 'nginx', 'caddy'):
            for label, path, hdr in endpoints:
                m = measure(target, path, hdr, a.requests, a.connections)
                results.setdefault(label, {})[target] = m
                print('%-8s %-20s %6d/s  p50 %6.2f ms  p95 %6.2f ms  p99 %6.2f ms  errors %d' % (target, label, m['per_second'], m['p50_ms'], m['p95_ms'], m['p99_ms'], m['errors']), flush=True)
    finally:
        subprocess.run(['docker', 'rm', '-f', 'cg-load-caddy'], capture_output=True)
        subprocess.run(['docker', 'rm', '-f', npt.NAME], capture_output=True)
        server.kill()
    o = ROOT / 'outputs' / 'defense' / 'proxy_load.json'
    o.parent.mkdir(parents=True, exist_ok=True)
    o.write_text(json.dumps({'connections': a.connections, 'requests_each': a.requests, 'results': results, 'note': 'one laptop, one python client; proxies use TLS, direct does not'}, indent=1), encoding='utf-8')
    print('wrote', o)
    return 0


if __name__ == '__main__':
    sys.exit(main())
