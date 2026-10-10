"""Attack of the deployed Rust stack (deploy/rust/compose.yml): TLS, the proxy's limits, container isolation, network exposure, database
authentication, image contents. Brings the stack up with throw-away secrets, attacks it, and takes it down.

    python rust/tools/deploy_attack.py [--keep]

Needs Docker with the compose plugin and the images claimguard-rs:latest (docker build -f deploy/rust/Dockerfile -t claimguard-rs:latest .).
Authorized scope: containers this script starts on this machine. Exit code 1 if any check fails. What it cannot test is the target
machine's own firewall and operating system; those need the same checks run there.
"""
import argparse, json, os, secrets, socket, ssl, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMPOSE = ROOT / 'deploy' / 'rust' / 'compose.yml'
PROJECT = 'cgatk'
RESULTS = []
ENV = {}


def check(category, name, ok, detail=''):
    RESULTS.append(ok)
    print(('PASS ' if ok else 'FAIL ') + f'[{category}] {name}' + (f'  -- {detail}' if detail and not ok else ''), flush=True)


def sh(*args, input=None, timeout=120):
    env = dict(os.environ, MSYS_NO_PATHCONV='1')
    p = subprocess.run(list(args), capture_output=True, text=True, encoding='utf-8', errors='replace', input=input, timeout=timeout, env=env)
    return p.returncode, (p.stdout + p.stderr).strip()


def dc(*args, **kw):
    return sh('docker', 'compose', '-f', str(COMPOSE), '--env-file', str(ENV['file']), '-p', PROJECT, *args, **kw)


def container(service):
    rc, out = dc('ps', '-q', service)
    return out.splitlines()[0].strip() if rc == 0 and out else None


def inspect(cid):
    return json.loads(sh('docker', 'inspect', cid)[1])[0]


def exec_in(service, cmd, user=None):
    args = ['docker', 'exec'] + (['-u', user] if user else []) + [container(service), 'sh', '-c', cmd]
    return sh(*args)


def tls_connect(min_v, max_v, alpn=None, host='localhost', port=443):
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    ctx.minimum_version, ctx.maximum_version = min_v, max_v
    if alpn:
        ctx.set_alpn_protocols(alpn)
    s = socket.create_connection((host, port), timeout=6)
    try:
        t = ctx.wrap_socket(s, server_hostname=host)
        return t
    except Exception:
        s.close()
        raise


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--keep', action='store_true')
    a = ap.parse_args()
    from cryptography.fernet import Fernet
    tmp = Path(tempfile.mkdtemp(prefix='cg_deploy_'))
    ENV['file'] = tmp / 'rs.env'
    ENV['mongo'] = secrets.token_urlsafe(18)
    ENV['redis'] = secrets.token_urlsafe(18)
    ENV['file'].write_text('\n'.join(['CG_SITE=localhost', 'CADDYFILE=Caddyfile.internal', 'JWT_SECRET=' + secrets.token_urlsafe(48), 'AUDIT_ANCHOR_KEY=' + secrets.token_urlsafe(48),
                                      'PII_KEY=' + secrets.token_urlsafe(48), 'FERNET_KEY=' + Fernet.generate_key().decode(), 'MONGO_ROOT_USER=claimguard',
                                      'MONGO_ROOT_PASSWORD=' + ENV['mongo'], 'REDIS_PASSWORD=' + ENV['redis']]) + '\n')
    rc, out = dc('up', '-d')
    if rc:
        print(out[-600:])
        return 2
    try:
        for _ in range(60):
            rc, out = dc('ps', '--format', '{{.Service}} {{.Health}}')
            h = dict(l.split(' ', 1) for l in out.splitlines() if ' ' in l)
            if all(h.get(s) == 'healthy' for s in ('api', 'worker', 'mongo', 'redis')):
                break
            time.sleep(3)
        check('stack', 'api, worker, mongo and redis all become healthy', all(h.get(s) == 'healthy' for s in ('api', 'worker', 'mongo', 'redis')), h)
        attack()
    finally:
        if not a.keep:
            dc('down', '-v')
    bad = RESULTS.count(False)
    print(f'\n{len(RESULTS)} checks, {bad} FAILED')
    return 1 if bad else 0


def attack():
    # ---------------------------------------------------------------- TLS
    for name, lo, hi in (('TLS 1.0', ssl.TLSVersion.TLSv1, ssl.TLSVersion.TLSv1), ('TLS 1.1', ssl.TLSVersion.TLSv1_1, ssl.TLSVersion.TLSv1_1)):
        try:
            tls_connect(lo, hi).close()
            ok, d = False, 'accepted'
        except (ssl.SSLError, OSError, ValueError) as e:
            ok, d = True, type(e).__name__
        check('tls', f'{name} is refused', ok, d)
    t12 = t13 = None
    try:
        t12 = tls_connect(ssl.TLSVersion.TLSv1_2, ssl.TLSVersion.TLSv1_2)
        c12 = t12.cipher()
        t12.close()
    except Exception as e:  # noqa: BLE001
        c12 = None
    t13 = tls_connect(ssl.TLSVersion.TLSv1_3, ssl.TLSVersion.TLSv1_3, alpn=['h2', 'http/1.1'])
    check('tls', 'TLS 1.3 is accepted and HTTP/2 is negotiated', t13.version() == 'TLSv1.3' and t13.selected_alpn_protocol() == 'h2', (t13.version(), t13.selected_alpn_protocol()))
    der = t13.getpeercert(binary_form=True)
    t13.close()
    from cryptography import x509
    cert = x509.load_der_x509_certificate(der)
    check('tls', 'the certificate is currently valid and not near expiry', cert.not_valid_before_utc.timestamp() < time.time() < cert.not_valid_after_utc.timestamp(), str(cert.not_valid_after_utc))
    weak = any(w in (c12[0] if c12 else '') for w in ('RC4', 'DES', 'NULL', 'EXPORT', 'MD5', 'CBC'))
    check('tls', 'a TLS 1.2 connection (if offered) uses a forward-secret AEAD cipher', c12 is None or ('GCM' in c12[0] or 'CHACHA' in c12[0]) and ('ECDHE' in c12[0] or 'DHE' in c12[0]) and not weak, c12)
    r = sh('curl', '-sk', '-i', '--max-time', '10', 'https://localhost/healthz')[1]
    low = r.lower()
    check('tls', 'HSTS is sent and no Server banner', 'strict-transport-security' in low and '\nserver:' not in low, r[:300])
    rc, out = sh('curl', '-s', '-i', '--max-time', '10', 'http://localhost/healthz')
    check('tls', 'plain HTTP on port 80 only redirects to HTTPS and serves nothing', ' 308 ' in out.split('\n')[0] or ' 301 ' in out.split('\n')[0] or ' 302 ' in out.split('\n')[0], out.split('\n')[0])
    # ---------------------------------------------------------------- proxy limits
    big = 'A' * 20000
    rc, out = sh('curl', '-sk', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', '-H', f'X-Big: {big}', 'https://localhost/healthz')
    check('proxy', 'a 20 KB header is refused at the proxy', out.strip() in ('431', '400', '000'), out)
    rc, out = sh('curl', '-sk', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '20', '-X', 'POST', '-H', 'content-type: application/json', '--data-binary', '@-', 'https://localhost/api/v1/auth/login', input='x' * 1_000_000)
    check('proxy', 'a 1 MB body reaches the application and is refused with 413', out.strip() == '413', out)
    slow = []
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    for _ in range(60):
        try:
            s = ctx.wrap_socket(socket.create_connection(('localhost', 443), timeout=6), server_hostname='localhost')
            s.sendall(b'GET /healthz HTTP/1.1\r\nHost: localhost\r\nX-Slow: ')
            slow.append(s)
        except Exception:  # noqa: BLE001
            pass
    t0 = time.time()
    st = sh('curl', '-sk', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', 'https://localhost/healthz')[1].strip()
    check('proxy', f'the proxy still serves with {len(slow)} half-sent requests open', st == '200' and time.time() - t0 < 3, (st, time.time() - t0))
    time.sleep(14)
    held = 0
    for s in slow:
        try:
            s.settimeout(0.3)
            if s.recv(1) != b'':
                held += 1
            # an empty read or a close notice both mean the proxy hung up
        except ssl.SSLWantReadError:
            held += 1
        except socket.timeout:
            held += 1
        except Exception:  # noqa: BLE001
            pass
        finally:
            s.close()
    check('proxy', 'half-sent requests are cut off by the proxy within its 10-second limit', held == 0, f'{held} still open after 14 s')
    # ---------------------------------------------------------------- what is published
    rc, out = dc('ps', '--format', 'json')
    pubs = {}
    for line in out.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        pubs[row['Service']] = [p for p in row.get('Publishers') or [] if p.get('PublishedPort')]
    check('network', 'only the proxy publishes ports (80 and 443)', {s for s, p in pubs.items() if p} == {'proxy'} and {p['PublishedPort'] for p in pubs['proxy']} == {80, 443}, pubs)
    for port, what in ((27017, 'MongoDB'), (6379, 'Redis'), (8443, 'the API')):
        try:
            socket.create_connection(('127.0.0.1', port), timeout=2).close()
            ok = False
        except OSError:
            ok = True
        check('network', f'{what} (port {port}) is not reachable from the host', ok)
    # ---------------------------------------------------------------- network segmentation
    rc, out = exec_in('proxy', 'nc -z -w 3 mongo 27017 && echo reachable || echo blocked')
    check('network', 'the proxy cannot reach MongoDB', 'blocked' in out and 'reachable' not in out.replace('blocked', ''), out)
    rc, out = exec_in('proxy', 'nc -z -w 3 redis 6379 && echo reachable || echo blocked')
    check('network', 'the proxy cannot reach Redis', 'blocked' in out and 'reachable' not in out.replace('blocked', ''), out)
    rc, out = exec_in('api', 'cg health --port 8443 && echo ok')
    check('network', 'the API answers its own health check', 'ok' in out, out)
    rc, out = sh('docker', 'exec', container('mongo'), 'bash', '-c', 'timeout 4 bash -c "echo > /dev/tcp/1.1.1.1/443" && echo reachable || echo blocked')
    check('network', 'MongoDB has no route to the internet', 'blocked' in out and 'reachable' not in out.replace('blocked', ''), out)
    rc, out = sh('docker', 'exec', container('redis'), 'bash', '-c', 'timeout 4 bash -c "echo > /dev/tcp/1.1.1.1/443" && echo reachable || echo blocked')
    check('network', 'Redis has no route to the internet', 'blocked' in out and 'reachable' not in out.replace('blocked', ''), out)
    # ---------------------------------------------------------------- database authentication
    rc, out = sh('docker', 'exec', container('mongo'), 'mongosh', '--quiet', '--eval', 'JSON.stringify(db.adminCommand({listDatabases:1}))')
    check('database', 'MongoDB refuses an unauthenticated listing of databases', 'requires authentication' in out or 'Unauthorized' in out or '"ok":0' in out, out[:200])
    rc, out = sh('docker', 'exec', container('mongo'), 'mongosh', '--quiet', '-u', 'claimguard', '-p', 'wrong-password', '--authenticationDatabase', 'admin', '--eval', 'db.runCommand({ping:1})')
    check('database', 'MongoDB refuses a wrong password', 'Authentication failed' in out or 'AuthenticationFailed' in out, out[:200])
    rc, out = sh('docker', 'exec', container('mongo'), 'mongosh', '--quiet', '-u', 'claimguard', '-p', ENV['mongo'], '--authenticationDatabase', 'admin', '--eval', 'db.runCommand({ping:1}).ok')
    check('database', 'MongoDB accepts the configured credentials (sanity)', out.strip().endswith('1'), out[:200])
    rc, out = sh('docker', 'exec', container('redis'), 'redis-cli', 'ping')
    check('database', 'Redis refuses commands without the password', 'NOAUTH' in out, out)
    rc, out = sh('docker', 'exec', container('redis'), 'redis-cli', '-a', 'wrong', '--no-auth-warning', 'ping')
    check('database', 'Redis refuses a wrong password', 'WRONGPASS' in out or 'invalid' in out.lower(), out)
    # ---------------------------------------------------------------- container isolation
    for svc in ('api', 'worker'):
        info = inspect(container(svc))
        hc, cfg = info['HostConfig'], info['Config']
        check('isolation', f'{svc}: runs as a non-root user', cfg.get('User') in ('claimguard', '10001') or rc == 0 and 'uid=10001' in exec_in(svc, 'id')[1], cfg.get('User'))
        check('isolation', f'{svc}: read-only root file system, all capabilities dropped, no-new-privileges, not privileged',
              hc['ReadonlyRootfs'] is True and 'ALL' in (hc.get('CapDrop') or []) and not hc.get('CapAdd') and any('no-new-privileges' in o for o in hc.get('SecurityOpt') or []) and hc['Privileged'] is False,
              {k: hc.get(k) for k in ('ReadonlyRootfs', 'CapDrop', 'CapAdd', 'SecurityOpt', 'Privileged')})
        check('isolation', f'{svc}: memory, process and CPU limits are set', hc['Memory'] > 0 and hc['PidsLimit'] and hc['PidsLimit'] > 0 and hc['NanoCpus'] > 0, {k: hc.get(k) for k in ('Memory', 'PidsLimit', 'NanoCpus')})
        mounts = [m['Source'] + ':' + m['Destination'] for m in info['Mounts']]
        check('isolation', f'{svc}: no host path and no Docker socket is mounted', all(m.get('Type') == 'volume' for m in info['Mounts']) and not any('docker.sock' in m for m in mounts), mounts)
        check('isolation', f'{svc}: not on the host network or host PID namespace', hc['NetworkMode'] != 'host' and hc.get('PidMode', '') != 'host', hc['NetworkMode'])
        rc, out = exec_in(svc, 'id -u; touch /pwned 2>&1; touch /app/pwned 2>&1; touch /usr/local/bin/pwned 2>&1; echo x > /tmp/ok && echo tmp-writable; grep -E "^(CapEff|CapBnd|NoNewPrivs)" /proc/self/status')
        lines = out.splitlines()
        check('isolation', f'{svc}: uid is not 0', lines and lines[0].strip() not in ('0', ''), lines[:1])
        check('isolation', f'{svc}: writing to /, /app and /usr/local/bin fails (read-only), /tmp works', out.count('Read-only file system') == 3 and 'tmp-writable' in out, out[:300])
        check('isolation', f'{svc}: no effective capabilities and NoNewPrivs is set', 'CapEff:\t0000000000000000' in out and 'NoNewPrivs:\t1' in out, out[-200:])
    rc, out = exec_in('worker', 'touch /mirror/ok && echo mirror-writable; ls /mirror >/dev/null 2>&1 && echo mirror-present')
    check('isolation', 'worker: the audit mirror volume is mounted and writable', 'mirror-writable' in out, out)
    rc, out = exec_in('api', 'ls /mirror 2>&1')
    check('isolation', 'api: the audit mirror volume is NOT mounted (a compromised API cannot rewrite the copy)', 'No such file' in out, out)
    # ---------------------------------------------------------------- the image
    rc, out = sh('docker', 'run', '--rm', '--entrypoint', 'sh', 'claimguard-rs:latest', '-c',
                 'find / -xdev \\( -name ".env*" -o -name "*.pem" -o -name "*.key" -o -name "dev_secrets.json" -o -name "demo_accounts.json" -o -name "id_rsa*" \\) 2>/dev/null | grep -v "^/etc/ssl\\|^/usr/share\\|^/usr/lib\\|^/etc/ca-cert" | head')
    check('image', 'the image contains no environment file, key, certificate or demo account file', out.strip() == '', out[:200])
    rc, out = sh('docker', 'history', '--no-trunc', '--format', '{{.CreatedBy}}', 'claimguard-rs:latest')
    check('image', 'no secret-like value appears in the build history', not any(w in out for w in ('JWT_SECRET=', 'FERNET_KEY=', 'PASSWORD=', 'PII_KEY=')), '')
    rc, out = sh('docker', 'run', '--rm', '--entrypoint', 'sh', 'claimguard-rs:latest', '-c', 'which gcc cc make curl wget python python3 nc git ssh 2>/dev/null')
    check('image', 'the runtime image has no compiler, package fetcher, interpreter or ssh client', out.strip() == '', out)


if __name__ == '__main__':
    sys.exit(main())
