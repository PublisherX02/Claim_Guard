"""Does the live-update stream get through the real reverse proxy (Caddy) without being held back?

    python rust/tools/sse_proxy_test.py [--caddyfile deploy/Caddyfile.internal]

Starts a demo server on the host (127.0.0.1:8443; Docker Desktop forwards the host gateway to it) and the pinned Caddy image in Docker with the given Caddyfile (internal
certificate, published on 127.0.0.1:9443), signs in through the proxy over TLS and opens the event stream. PASS needs: the first message
("hello") arrives within 2 seconds (a buffering or compressing proxy would hold it back), the response is not compressed, a change is
announced, and an ordinary request is still answered. Needs Docker and the caddy image (the one in deploy/rust/compose.yml); demo scope only.
"""
import argparse
import json
import os
import socket
import ssl
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pyotp  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
IMAGE = 'caddy:2@sha256:f2a1290d0463aad60660d4ec134943f183ee2a5f6c3eb7bf32dd984f2f020772'
NAME = 'cgsse-caddy'
CRLF = bytes([13, 10])
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def tls_socket():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE        # the proxy's own internal authority; this test only talks to its own container on 127.0.0.1
    raw = socket.create_connection(('127.0.0.1', 9443), timeout=10)
    return ctx.wrap_socket(raw, server_hostname='localhost')


def request(method, path, body=None, cookie=None, csrf=None, stream=False):
    # only the stream is asked for compression: the proxy must still leave it uncompressed, while other responses stay readable here
    s = tls_socket()
    # only the stream asks for compression: the proxy must still leave it uncompressed, while other responses stay readable here
    head = '%s %s HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: %s\r\n' % (method, path, 'gzip, zstd' if stream else 'identity')
    data = b''
    if body is not None:
        data = json.dumps(body).encode()
        head += 'Content-Type: application/json\r\nContent-Length: %d\r\n' % len(data)
    if cookie:
        head += 'Cookie: %s\r\n' % cookie
    if csrf:
        head += 'X-CSRF-Token: %s\r\n' % csrf
    s.sendall(head.encode() + (b'Connection: close\r\n' if not stream else b'') + CRLF + data)
    return s


def read_response(s, until_body=False, timeout=8):
    s.settimeout(timeout)
    buf = b''
    while CRLF + CRLF not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    head, _, rest = buf.partition(CRLF + CRLF)
    lines = head.decode('latin1').split('\r\n')
    status = int(lines[0].split()[1])
    headers = {}
    for l in lines[1:]:
        k, _, v = l.partition(':')
        headers.setdefault(k.strip().lower(), []).append(v.strip())
    while not until_body:
        try:
            chunk = s.recv(65536)
        except OSError:
            break
        if not chunk:
            break
        rest += chunk
    return status, headers, rest


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--caddyfile', default=str(ROOT / 'deploy' / 'Caddyfile.internal'))
    a = ap.parse_args()
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_sse_proxy'
    data.mkdir(parents=True, exist_ok=True)
    subprocess.run(['docker', 'rm', '-f', NAME], capture_output=True)
    server = subprocess.Popen([CG, 'serve', '--demo', '--port', '8443', '--data-dir', str(data)],
                              env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen('http://127.0.0.1:8443/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        r = subprocess.run(['docker', 'run', '-d', '--name', NAME, '-p', '127.0.0.1:9443:443', '--add-host', 'api:host-gateway', '-e', 'CG_SITE=localhost',
                            '-v', '%s:/etc/caddy/Caddyfile:ro' % a.caddyfile, IMAGE], capture_output=True, text=True)
        if r.returncode != 0:
            print('docker run failed:', r.stderr[-300:])
            return 2
        for _ in range(60):
            try:
                s = request('GET', '/healthz')
                st, _, _ = read_response(s)
                if st == 200:
                    break
            except Exception:  # noqa: BLE001
                time.sleep(1)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        time.sleep(4)
        badge = 'CG-2002'
        tp = pyotp.TOTP(acc[badge]['secret'])
        s = request('POST', '/api/v1/auth/login', {'badge_id': badge, 'password': acc[badge]['password'], 'totp': tp.now()})
        st, h, body = read_response(s)
        check('sign-in through the proxy', st == 200, (st, body[:100]))
        cookie = 'cg_session=' + h['set-cookie'][0].split('cg_session=')[1].split(';')[0] if any('cg_session=' in c for c in h.get('set-cookie', [])) else None
        csrf = json.loads(body)['csrf']
        t0 = time.time()
        s = request('GET', '/api/v1/work/events?topics=inbox', cookie=cookie, stream=True)
        st, h, rest = read_response(s, until_body=True)
        got = rest
        end = time.time() + 2
        while b'event: hello' not in got and time.time() < end:
            s.settimeout(max(0.1, end - time.time()))
            try:
                got += s.recv(65536)
            except OSError:
                break
        first = time.time() - t0
        check('stream opens through the proxy as text/event-stream', st == 200 and h.get('content-type', [''])[0].startswith('text/event-stream'), (st, h))
        check('hello arrives within 2 s (not held back by the proxy)', b'event: hello' in got, (first, got[:120]))
        check('the stream is not compressed', 'content-encoding' not in h, h.get('content-encoding'))
        check('security headers survive the proxy (HSTS)', 'strict-transport-security' in h, list(h))
        # a change: clear a green claim on a second connection, expect "changed" through the proxy
        s2 = request('GET', '/api/v1/work/inbox', cookie=cookie)
        st2, _, b2 = read_response(s2)
        green = [c['claim_id'] for c in json.loads(b2)['claims'] if c.get('lane') == 'green'] if st2 == 200 else []
        if green:
            s3 = request('POST', '/api/v1/work/claims/%s/verify' % green[0], {'action': 'verify_clear'}, cookie=cookie, csrf=csrf)
            check('a claim is cleared', read_response(s3)[0] == 200)
            end = time.time() + 9
            while b'event: changed' not in got and time.time() < end:
                s.settimeout(max(0.1, end - time.time()))
                try:
                    got += s.recv(65536)
                except OSError:
                    break
            check('"changed" arrives through the proxy within 9 s', b'event: changed' in got, got[-200:])
        else:
            check('a green claim was available', False, b2[:200])
        s4 = request('GET', '/api/v1/auth/me', cookie=cookie)
        check('an ordinary request is still answered while the stream is open', read_response(s4)[0] == 200)
        s.close()
    finally:
        subprocess.run(['docker', 'rm', '-f', NAME], capture_output=True)
        server.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
