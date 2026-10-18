#!/usr/bin/env python3
"""deploy/nginx/nginx.conf in front of the real server, over TLS (docs/54). LOCAL DEMO scope.

    python rust/tools/nginx_proxy_test.py [--cg path/to/cg.exe]

Needs Docker, the nginx image (pulled once: nginx:1.27-alpine) and the python `cryptography` package (a throwaway certificate). Starts the demo server on the host (127.0.0.1:8443; Docker Desktop forwards the host gateway to it)
and nginx in Docker with the config (published on 127.0.0.1:9444 and :9081), then checks:
  phase A, a tiny echo server in place of the application: the client address the application is given is nginx's, never one the client wrote
  phase B, the real server: TLS and headers, compression, the per-path body limits (100 KB, 4 MB, 17 MB), a batch of 1,000 claims through the proxy,
          the live-update stream, a slow client being cut off, the rate limits (general and sign-in), and a clean JSON answer when the application is down
Writes outputs/defense/nginx_proxy_test.json.
"""
import argparse
import http.client
import json
import os
import socket
import ssl
import subprocess
import sys
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pyotp  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
IMAGE = 'nginx:1.27-alpine'
NAME = 'cg-nginx-test'
HTTPS, HTTP = 9444, 9081
LOG, FAILED = [], []
CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE        # the proxy's own throwaway certificate, on 127.0.0.1


def check(name, ok, detail=''):
    LOG.append({'check': name, 'ok': bool(ok)})
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def call(method, path, body=None, headers=None, cookie=None, csrf=None, timeout=30, raw_body=None):
    c = http.client.HTTPSConnection('127.0.0.1', HTTPS, context=CTX, timeout=timeout)
    h = {'Host': 'localhost:%d' % HTTPS, 'Accept-Encoding': 'identity'}
    h.update(headers or {})
    data = raw_body
    if body is not None:
        data = json.dumps(body).encode()
    if data is not None:
        h['Content-Type'] = 'application/json'
    if cookie:
        h['Cookie'] = cookie
    if csrf:
        h['X-CSRF-Token'] = csrf
    try:
        c.request(method, path, body=data, headers=h)
        r = c.getresponse()
        b = r.read()
        hd = {}
        for k, v in r.getheaders():
            hd[k.lower()] = hd[k.lower()] + ' | ' + v if k.lower() in hd else v       # a repeated header (Set-Cookie) is kept whole
        try:
            return r.status, hd, json.loads(b)
        except ValueError:
            return r.status, hd, b
    except (OSError, http.client.HTTPException) as e:
        return None, {}, repr(e)
    finally:
        c.close()


def docker_run(conf, extra=()):
    subprocess.run(['docker', 'rm', '-f', NAME], capture_output=True)
    r = subprocess.run(['docker', 'run', '-d', '--name', NAME, '-p', '127.0.0.1:%d:443' % HTTPS, '-p', '127.0.0.1:%d:80' % HTTP, '--add-host', 'api:host-gateway',
                        '-v', '%s:/etc/nginx/nginx.conf:ro' % conf, '-v', '%s:/etc/nginx/tls:ro' % TLS, *extra, IMAGE], capture_output=True, text=True)
    if r.returncode != 0:
        print('docker run failed:', r.stderr[-400:])
        sys.exit(2)
    for _ in range(40):
        s, _, _ = call('GET', '/healthz', timeout=3)
        if s is not None:
            return
        time.sleep(0.5)
    print(subprocess.run(['docker', 'logs', NAME], capture_output=True, text=True).stderr[-600:])
    sys.exit(2)


class Echo(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        body = json.dumps({k.lower(): v for k, v in self.headers.items()}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


def phase_a(conf):
    srv = ThreadingHTTPServer(('0.0.0.0', 8443), Echo)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        docker_run(conf)
        st, _, got = call('GET', '/echo', headers={'X-Forwarded-For': '6.6.6.6, 7.7.7.7', 'X-Real-IP': '8.8.8.8', 'X-Forwarded-Proto': 'http'})
        check('A: the request reaches the application through nginx', st == 200 and isinstance(got, dict), (st, got))
        xff = got.get('x-forwarded-for', '') if isinstance(got, dict) else ''
        check('A: what the client wrote in X-Forwarded-For is thrown away (one address, nginx\'s own view)', xff != '' and ',' not in xff and '6.6.6.6' not in xff and '7.7.7.7' not in xff, xff)
        check('A: X-Real-IP is nginx\'s view too, not the client\'s', got.get('x-real-ip') == xff, (got.get('x-real-ip'), xff))
        check('A: the protocol the application is told is https, whatever the client wrote', got.get('x-forwarded-proto') == 'https', got.get('x-forwarded-proto'))
        check('A: the Host header is passed on', got.get('host') == 'localhost:%d' % HTTPS, got.get('host'))
    finally:
        srv.shutdown()
        srv.server_close()
        subprocess.run(['docker', 'rm', '-f', NAME], capture_output=True)


def stream_checks(cookie, csrf):
    s = socket.create_connection(('127.0.0.1', HTTPS), timeout=10)
    s = CTX.wrap_socket(s, server_hostname='localhost')
    t0 = time.time()
    s.sendall(('GET /api/v1/work/events?topics=inbox HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: gzip\r\nCookie: %s\r\n\r\n' % cookie).encode())
    buf = b''
    end = time.time() + 3
    while b'event: hello' not in buf and time.time() < end:
        s.settimeout(max(0.1, end - time.time()))
        try:
            buf += s.recv(65536)
        except OSError:
            break
    head = buf.split(b'\r\n\r\n')[0].decode('latin1').lower()
    check('B: the stream opens as text/event-stream', buf.startswith(b'HTTP/1.1 200') and 'text/event-stream' in head, head[:200])
    check('B: "hello" arrives within 2 s (nothing holds it back)', b'event: hello' in buf and time.time() - t0 < 2.5, time.time() - t0)
    check('B: the stream is not compressed even when the client asks', 'content-encoding' not in head, head)
    st, _, b = call('GET', '/api/v1/auth/me', cookie=cookie)
    check('B: an ordinary request is answered while the stream is open', st == 200)
    s.close()


def phase_b(conf, cg, data):
    docker_run(conf)
    acc = json.loads((data / 'demo_accounts.json').read_text())
    tp = pyotp.TOTP(acc['CG-4004']['secret'])
    st, h, body = call('POST', '/api/v1/auth/login', {'badge_id': 'CG-4004', 'password': acc['CG-4004']['password'], 'totp': tp.now()})
    check('B: sign-in through nginx over TLS', st == 200 and isinstance(body, dict), (st, body))
    cookie = 'cg_session=' + h['set-cookie'].split('cg_session=')[1].split(';')[0]
    csrf = body['csrf']
    check('B: HSTS is sent', 'max-age=31536000' in h.get('strict-transport-security', ''), h)
    check('B: the nginx version number is not shown', 'nginx/' not in h.get('server', ''), h.get('server'))

    # the plain HTTP port only redirects
    c = http.client.HTTPConnection('127.0.0.1', HTTP, timeout=5)
    c.request('GET', '/api/v1/auth/me', headers={'Host': 'localhost'})
    r = c.getresponse()
    check('B: port 80 redirects to https', r.status == 301 and r.getheader('Location', '').startswith('https://'), (r.status, r.getheader('Location')))
    c.close()

    # compression: the console files are compressed, JSON too
    st, h2, _ = call('GET', '/ui/app.js', headers={'Accept-Encoding': 'gzip'})
    check('B: the console files are compressed for a client that accepts it', st == 200 and h2.get('content-encoding') == 'gzip', (st, h2.get('content-encoding'), h2.get('content-type')))

    # body limits per path
    st, _, b = call('POST', '/api/v1/work/claims/x/verify', raw_body=b'{"a":"' + b'x' * 120_000 + b'"}', cookie=cookie, csrf=csrf)
    check('B: 120 KB to an ordinary path is refused at the proxy with a JSON 413', st == 413 and b == {'error': 'payload_too_large'}, (st, b))
    st, _, b = call('POST', '/api/v1/work/claims/x/verify', raw_body=b'{"a":"' + b'x' * 50_000 + b'"}', cookie=cookie, csrf=csrf)
    check('B: 50 KB to the same path gets through to the application (the application itself refuses over 64 KB)', st is not None and st != 413, (st, str(b)[:100]))
    st, _, b = call('POST', '/api/v1/queue/submit', raw_body=b'{"claims":[],"pad":"' + b'x' * 5_000_000 + b'"}', cookie=cookie, csrf=csrf)
    check('B: 5 MB to the submit path is refused (413)', st == 413, (st, str(b)[:100]))
    st, _, b = call('POST', '/api/v1/queue/import/file', raw_body=b'x' * 18_000_000, cookie=cookie, csrf=csrf, timeout=60, headers={'Content-Type': 'application/octet-stream'})
    check('B: 18 MB to an import path is refused (413)', st == 413, (st, str(b)[:100]))
    st, _, b = call('POST', '/api/v1/queue/import/file', raw_body=b'x' * 1_000_000, cookie=cookie, csrf=csrf, timeout=60, headers={'Content-Type': 'application/octet-stream'})
    check('B: 1 MB to an import path reaches the application', st is not None and st != 413, (st, str(b)[:100]))

    # auto-clear on, then a batch of 1,000 claims through the proxy
    st, _, cfg = call('GET', '/api/v1/queue/config', cookie=cookie)
    st, _, _ = call('PATCH', '/api/v1/queue/config', {'expected_version': cfg['stored_version'], 'auto_clear_green': True, 'audit_percent': 5}, cookie=cookie, csrf=csrf)
    claims = [json.loads(l) for l in (ROOT / 'data/validation/claims.jsonl').read_text(encoding='utf-8').splitlines() if l.strip()]
    expected = [json.loads(l) for l in (ROOT / 'data/validation/expected_results.jsonl').read_text(encoding='utf-8').splitlines() if l.strip()]
    bad = {e['claim_id'] for e in expected if e['status'] in ('FAIL', 'UNABLE_TO_ASSESS')}
    base = [c for c in claims if c['claim_id'] not in bad]
    big = [json.loads(json.dumps(base[i % len(base)]).replace(base[i % len(base)]['claim_id'], 'CG-N%011X' % i)) for i in range(1000)]
    size = len(json.dumps({'claims': big}))
    t0 = time.time()
    st, _, out = call('POST', '/api/v1/queue/submit', {'claims': big}, cookie=cookie, csrf=csrf, timeout=180)
    took = time.time() - t0
    check('B: a batch of 1,000 claims (%.1f MB) goes through nginx in one request (%.1f s)' % (size / 1e6, took), st == 200 and len(out.get('results', [])) == 1000 and all(r.get('accepted') for r in out['results']), (st, str(out)[:150]))
    st, _, b = call('GET', '/api/v1/queue/batches', cookie=cookie)
    rows = [x for x in b.get('batches', []) if x['claims'] == 1000] if isinstance(b, dict) else []
    check('B: it is one batch with an exact 5% sample', len(rows) == 1 and rows[0]['sampled'] == max(1, -(-rows[0]['green'] * 5 // 100)), rows)

    # the stream is a reviewer's (the administrator has no inbox to watch)
    tp2 = pyotp.TOTP(acc['CG-2002']['secret'])
    st, h3, body3 = call('POST', '/api/v1/auth/login', {'badge_id': 'CG-2002', 'password': acc['CG-2002']['password'], 'totp': tp2.now()})
    check('B: a reviewer signs in through nginx', st == 200, (st, body3))
    stream_checks('cg_session=' + h3['set-cookie'].split('cg_session=')[1].split(';')[0], body3['csrf'])

    # a client that sends a header and then nothing is cut off
    s = CTX.wrap_socket(socket.create_connection(('127.0.0.1', HTTPS), timeout=20), server_hostname='localhost')
    s.sendall(b'GET /healthz HTTP/1.1\r\nHost: localhost\r\nX-Slow: ')
    t0 = time.time()
    s.settimeout(15)
    closed = False
    try:
        closed = s.recv(1024) in (b'', ) or True       # an answer (408) or a close both mean it was cut off
    except OSError:
        closed = True
    waited = time.time() - t0
    check('B: a client that stalls in the middle of its header is cut off after about 10 s (%.1f s)' % waited, closed and 8 <= waited <= 13, waited)
    s.close()

    # rate limits: ordinary traffic passes; a flood is slowed
    codes = [call('GET', '/healthz')[0] for _ in range(40)]
    check('B: 40 ordinary requests in a row are all answered', all(c == 200 for c in codes), codes)
    time.sleep(3)
    with ThreadPoolExecutor(20) as ex:
        codes = list(ex.map(lambda _: call('GET', '/healthz')[0], range(400)))
    n429 = codes.count(429)
    check('B: a flood of 400 requests is slowed with 429 (%d refused, %d answered)' % (n429, codes.count(200)), n429 > 0 and codes.count(200) > 60, (n429, codes.count(200)))
    time.sleep(4)
    check('B: and normal service resumes a few seconds later', call('GET', '/healthz')[0] == 200)
    time.sleep(1)
    res = [call('POST', '/api/v1/auth/login', {'badge_id': 'CG-9999', 'password': 'not-the-password', 'totp': '000000'}) for _ in range(14)]
    codes = [r[0] for r in res]
    check('B: repeated sign-in attempts hit the sign-in limit (429) after a short burst', codes.count(429) >= 5 and 401 in codes[:3], codes)
    last429 = [r for r in res if r[0] == 429]
    check('B: the 429 is JSON with Retry-After', bool(last429) and last429[0][2] == {'error': 'too_many_requests'} and 'retry-after' in last429[0][1], last429[:1])
    return took, size


def make_certificate(folder):
    """A throwaway self-signed certificate for localhost, valid two days (this test only)."""
    import datetime
    import ipaddress
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=5)).not_valid_after(now + datetime.timedelta(days=2))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost'), x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]), critical=False)
            .sign(key, hashes.SHA256()))
    (folder / 'key.pem').write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.TraditionalOpenSSL, serialization.NoEncryption()))
    (folder / 'cert.pem').write_bytes(cert.public_bytes(serialization.Encoding.PEM))


def main():
    ap = argparse.ArgumentParser()
    rel = ROOT / 'rust' / 'target' / 'release' / 'cg.exe'
    ap.add_argument('--cg', default=str(rel if rel.exists() else ROOT / 'rust' / 'target' / 'debug' / 'cg.exe'))
    a = ap.parse_args()
    global TLS
    work = Path(os.environ.get('TEMP', '/tmp')) / 'cg_nginx_test'
    TLS = work / 'tls'
    TLS.mkdir(parents=True, exist_ok=True)
    make_certificate(TLS)
    conf = str(ROOT / 'deploy' / 'nginx' / 'nginx.conf')
    t = subprocess.run(['docker', 'run', '--rm', '--add-host', 'api:host-gateway', '-v', '%s:/etc/nginx/nginx.conf:ro' % conf, '-v', '%s:/etc/nginx/tls:ro' % TLS, IMAGE, 'nginx', '-t'], capture_output=True, text=True)
    check('the configuration is valid (nginx -t)', t.returncode == 0, t.stderr[-300:])
    phase_a(conf)
    data = work / 'data'
    data.mkdir(exist_ok=True)
    for f in data.iterdir():
        if f.is_file():
            f.unlink()
    server = subprocess.Popen([a.cg, 'serve', '--demo', '--queue', '--port', '8443', '--data-dir', str(data)], env=dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), CG_DEMO_CLAIMS='0'),
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    extra = {}
    try:
        for _ in range(120):
            try:
                urllib.request.urlopen('http://127.0.0.1:8443/healthz', timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        took, size = phase_b(conf, a.cg, data)
        extra = {'batch_1000_seconds_through_nginx': round(took, 1), 'batch_1000_bytes': size}
        server.kill()
        server.wait()
        time.sleep(1)
        st, h, b = call('GET', '/api/v1/auth/me')
        check('B: with the application down, the answer is a clean JSON 503', st == 503 and b == {'error': 'unavailable'}, (st, str(b)[:100]))
    finally:
        subprocess.run(['docker', 'rm', '-f', NAME], capture_output=True)
        server.kill()
    o = ROOT / 'outputs' / 'defense' / 'nginx_proxy_test.json'
    o.parent.mkdir(parents=True, exist_ok=True)
    o.write_text(json.dumps({'image': IMAGE, **extra, 'checks': LOG}, indent=1), encoding='utf-8')
    print('wrote', o)
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
