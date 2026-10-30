#!/usr/bin/env python3
"""S6. What does the server show to the network? Run this from ANOTHER machine (the admin PC), never from the server itself (docs/65).

    python experiments/server/s06_exposure.py --run <name> --host <server-ip-or-name> [--ssh-port 22]

Checks, all read-only and polite (one connection at a time, no flood, no sign-in attempts):
  * ports: only 80, 443 (and 22 from the admin PC) accept a connection; the database (27017), the queue (6379), the API (8080/8000), the embedding
    service (8091), the document reader (8095), the model server (8000/11434) must NOT be reachable from outside;
  * TLS: protocols older than TLS 1.2 are refused, the certificate is read (subject, issuer, days left);
  * HTTP on port 80 redirects to HTTPS and shows nothing else;
  * headers: HSTS, X-Content-Type-Options, X-Frame-Options or a frame-ancestors rule, Content-Security-Policy, no Server/X-Powered-By version;
  * an anonymous request to the API and to the health panel is refused (401), and /healthz says nothing but that it is up.
Only for a server you own. It does not try to break in; the penetration tests of docs/46 do that, on a throw-away copy.
"""
import argparse
import http.client
import json
import socket
import ssl
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import Run  # noqa: E402

MUST_BE_CLOSED = {27017: 'MongoDB', 6379: 'Redis', 8080: 'API', 8000: 'model server', 8091: 'embedding service', 8095: 'document reader', 11434: 'model server (Ollama)', 3000: 'app', 5432: 'database'}
EXPECTED_OPEN = {80: 'HTTP (redirect only)', 443: 'HTTPS'}


def is_open(host, port, timeout=2.0):
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def tls_info(host):
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
    out = {}
    try:
        with socket.create_connection((host, 443), timeout=5) as s, ctx.wrap_socket(s, server_hostname=host) as t:
            out['negotiated'] = t.version()
            der = t.getpeercert(binary_form=True)
    except OSError as e:
        return {'error': str(e)[:100]}
    try:
        import ssl as _s
        pem = _s.DER_cert_to_PEM_cert(der)
        import tempfile
        with tempfile.NamedTemporaryFile('w', suffix='.pem', delete=False) as f:
            f.write(pem)
        d = _s._ssl._test_decode_cert(f.name)  # noqa: SLF001 - stdlib way to read a certificate without extra packages
        out['subject'] = ' '.join(x[0][1] for x in d.get('subject', ()))
        out['issuer'] = ' '.join(x[0][1] for x in d.get('issuer', ()))
        end = datetime.strptime(d['notAfter'], '%b %d %H:%M:%S %Y %Z').replace(tzinfo=timezone.utc)
        out['days_left'] = (end - datetime.now(timezone.utc)).days
    except Exception as e:  # noqa: BLE001 - the certificate details are a bonus
        out['certificate_unreadable'] = str(e)[:80]
    old = {}
    for name, ver in (('TLS1.0', ssl.TLSVersion.TLSv1), ('TLS1.1', ssl.TLSVersion.TLSv1_1)):
        c = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        c.check_hostname, c.verify_mode = False, ssl.CERT_NONE
        try:
            c.minimum_version = c.maximum_version = ver
            with socket.create_connection((host, 443), timeout=5) as s, c.wrap_socket(s, server_hostname=host):
                old[name] = 'accepted'
        except (ssl.SSLError, OSError, ValueError):
            old[name] = 'refused'
    out['old_protocols'] = old
    return out


def fetch(host, port, path, tls):
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
    c = http.client.HTTPSConnection(host, port, timeout=8, context=ctx) if tls else http.client.HTTPConnection(host, port, timeout=8)
    try:
        c.request('GET', path)
        r = c.getresponse()
        return r.status, {k.lower(): v for k, v in r.getheaders()}, r.read(400)
    except (OSError, http.client.HTTPException) as e:
        return None, {}, str(e).encode()[:100]
    finally:
        c.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--host', required=True)
    ap.add_argument('--ssh-port', type=int, default=22)
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run = Run(a.run, a.dry)
    ports = {p: {'service': n, 'open': is_open(a.host, p)} for p, n in {**MUST_BE_CLOSED, **EXPECTED_OPEN, a.ssh_port: 'SSH'}.items()}
    tls = tls_info(a.host)
    s80, h80, _ = fetch(a.host, 80, '/', False)
    s443, h443, _ = fetch(a.host, 443, '/', True)
    anon_api, _, _ = fetch(a.host, 443, '/api/v1/claims', True)
    anon_health, _, _ = fetch(a.host, 443, '/api/v1/ops/health', True)
    hz_status, _, hz_body = fetch(a.host, 443, '/healthz', True)
    headers = {'strict-transport-security': 'strict-transport-security' in h443, 'x-content-type-options': 'x-content-type-options' in h443,
               'content-security-policy': 'content-security-policy' in h443,
               'framing_protected': 'x-frame-options' in h443 or 'frame-ancestors' in h443.get('content-security-policy', ''),
               'no_server_version': not any(ch.isdigit() for ch in h443.get('server', '')) and 'x-powered-by' not in h443}
    exposed = [p for p in MUST_BE_CLOSED if ports[p]['open']]
    checks = {'internal_ports_closed': not exposed, 'https_open': ports[443]['open'], 'http_redirects_to_https': s80 in (301, 302, 307, 308) and h80.get('location', '').startswith('https://'),
              'old_tls_refused': tls.get('old_protocols', {}).get('TLS1.0') == 'refused' and tls.get('old_protocols', {}).get('TLS1.1') == 'refused',
              'security_headers_present': all(headers.values()), 'anonymous_api_refused': anon_api == 401, 'anonymous_health_panel_refused': anon_health == 401,
              'healthz_is_minimal': hz_status == 200 and len(hz_body) < 200}
    run.write('s06_exposure', {'host': a.host, 'ports': ports, 'exposed_internal_ports': exposed, 'tls': tls, 'http_status_port_80': s80, 'https_status': s443, 'headers': headers,
                               'anonymous': {'api_claims': anon_api, 'ops_health': anon_health}, 'checks': checks})
    print(json.dumps(checks, indent=1))


if __name__ == '__main__':
    main()
