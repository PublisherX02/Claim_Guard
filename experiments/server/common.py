"""Shared helpers of the server-side experiment suite (docs/65). Standard library only, so it runs on the target server without installing anything.

Sign-in uses two dedicated benchmark accounts, created for the experiments and deleted afterwards, each with the least permission that does the job:
an administrator (submits claims, imports documents, reads the health panel and the queue) and a read-only viewer (level 1: reads claims and
asks the rulebook). Their credentials come from the environment, never from the command line:

    CG_URL=https://<server>  CG_BADGE=CG-4xxx  CG_PASSWORD=...  CG_TOTP_SEED=<base32 authenticator seed>          (administrator)
    CG_VIEW_BADGE=CG-1xxx  CG_VIEW_PASSWORD=...  CG_VIEW_TOTP_SEED=<base32 authenticator seed>                      (viewer)
"""
import base64
import hashlib
import hmac
import http.cookiejar
import json
import os
import platform
import shutil
import ssl
import statistics
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT_ROOT = ROOT / 'outputs' / 'server'


def totp(seed, at=None, digits=6, step=30):
    """RFC 6238 one-time code (SHA-1), the kind every authenticator app shows."""
    key = base64.b32decode(seed.replace(' ', '').upper() + '=' * (-len(seed.replace(' ', '')) % 8))
    counter = struct.pack('>Q', int((time.time() if at is None else at) // step))
    h = hmac.new(key, counter, hashlib.sha1).digest()
    o = h[-1] & 15
    return str((struct.unpack('>I', h[o:o + 4])[0] & 0x7fffffff) % 10 ** digits).zfill(digits)


def pct(values, p):
    """The p-th percentile (0 to 100) by linear interpolation; None for an empty list."""
    v = sorted(values)
    if not v:
        return None
    k = (len(v) - 1) * p / 100
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def summary(values):
    """count, mean and the percentiles the report plots."""
    if not values:
        return {'n': 0}
    return {'n': len(values), 'mean': statistics.fmean(values), 'p50': pct(values, 50), 'p95': pct(values, 95), 'p99': pct(values, 99), 'max': max(values)}


def now_iso():
    return datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')


class Api:
    """A signed-in session with CSRF handling. `insecure` skips certificate checks (the internal certificate of an `internal` install)."""

    def __init__(self, base=None, badge=None, password=None, seed=None, insecure=True, timeout=60, prefix='CG_'):
        self.base = (base or os.environ.get('CG_URL', '')).rstrip('/')
        self.badge, self.password, self.seed = badge or os.environ.get(prefix + 'BADGE'), password or os.environ.get(prefix + 'PASSWORD'), seed or os.environ.get(prefix + 'TOTP_SEED')
        if not (self.base and self.badge and self.password and self.seed):
            who = 'a benchmark viewer (level 1)' if prefix == 'CG_VIEW_' else 'a benchmark administrator'
            raise SystemExit(f'set CG_URL, {prefix}BADGE, {prefix}PASSWORD and {prefix}TOTP_SEED ({who}; see docs/65 section 3)')
        ctx = ssl.create_default_context()
        if insecure:
            ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
        self.timeout, self.ctx = timeout, ctx
        self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()), urllib.request.HTTPSHandler(context=ctx))
        self.csrf = None

    def login(self):
        last = None
        for _ in range(4):
            status, body, _ = self.call('POST', '/api/v1/auth/login', {'badge_id': self.badge, 'password': self.password, 'totp': totp(self.seed)}, auth=False)
            if status == 200:
                self.csrf = body['csrf']
                return self
            last = (status, body)
            time.sleep(31)  # the same one-time code cannot be used twice in one 30 second step
        raise SystemExit(f'sign-in failed on {self.base}: {last}')

    def call(self, method, path, body=None, raw=None, headers=None, auth=True):
        """(status, parsed JSON or text, seconds). `raw` sends bytes as they are (file imports). A 401 after a service restart signs in once more."""
        out = self._call(method, path, body, raw, headers, auth)
        if out[0] == 401 and auth and self.csrf:
            try:
                self.login()
            except SystemExit:
                return out
            return self._call(method, path, body, raw, headers, auth)
        return out

    def healthy(self):
        """True when GET /healthz answers 200 (no sign-in needed)."""
        try:
            with urllib.request.urlopen(urllib.request.Request(self.base + '/healthz'), timeout=3, context=self.ctx) as r:
                return r.status == 200
        except (urllib.error.URLError, OSError, ValueError):
            return False

    def _call(self, method, path, body=None, raw=None, headers=None, auth=True):
        h = {'content-type': 'application/json'} if raw is None else {'content-type': 'application/octet-stream'}
        if auth and self.csrf and method not in ('GET', 'HEAD'):
            h['x-csrf-token'] = self.csrf
        h.update(headers or {})
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        t0 = time.perf_counter()
        try:
            r = self.op.open(urllib.request.Request(self.base + path, data, h, method=method), timeout=self.timeout)
            status, text = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, text = e.code, e.read()
        except (TimeoutError, OSError) as e:
            return 'ERROR', str(e)[:120], time.perf_counter() - t0
        dt = time.perf_counter() - t0
        try:
            return status, json.loads(text), dt
        except ValueError:
            return status, text[:200].decode('utf-8', 'replace'), dt


class Run:
    """One experiment run: a folder under outputs/server/<name>/ with one JSON file per experiment and env.json describing the machine."""

    def __init__(self, name=None, dry=False):
        self.name = name or datetime.now().strftime('%Y%m%d_%H%M%S')
        self.dir = OUT_ROOT / self.name
        self.dir.mkdir(parents=True, exist_ok=True)
        self.dry = dry

    def write(self, key, obj):
        obj = {'experiment': key, 'finished': now_iso(), 'dry_run': self.dry, **obj}
        (self.dir / f'{key}.json').write_text(json.dumps(obj, indent=1, ensure_ascii=False), encoding='utf-8')
        print(f'  wrote {self.dir / (key + ".json")}')

    def read(self, key):
        p = self.dir / f'{key}.json'
        return json.loads(p.read_text(encoding='utf-8')) if p.exists() else None


def sh(cmd, timeout=60, check=False):
    """Run a command, return (exit code, stdout). Never raises for a missing program."""
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, shell=isinstance(cmd, str))
        return p.returncode, p.stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as e:
        if check:
            raise
        return 127, str(e)


def parse_size(text):
    """'1.5GiB' or '812.3MiB' or '2GB' to bytes."""
    t = text.strip()
    units = {'B': 1, 'KB': 1e3, 'MB': 1e6, 'GB': 1e9, 'KIB': 1024, 'MIB': 1024 ** 2, 'GIB': 1024 ** 3, 'TIB': 1024 ** 4}
    for u in sorted(units, key=len, reverse=True):
        if t.upper().endswith(u):
            return int(float(t[:-len(u)]) * units[u])
    return int(float(t))


def fresh_claim(claim):
    """A copy of a synthetic claim under a new claim id, so a repeat run is new work and not a duplicate."""
    import copy
    import secrets
    c = copy.deepcopy(claim)
    cid = 'CG-' + secrets.token_hex(6).upper()
    c['claim_id'] = cid
    if 'invoice_number' in c:
        c['invoice_number'] = 'INV-' + cid
    if isinstance(c.get('coverage'), dict) and 'coverage_id' in c['coverage']:
        c['coverage']['coverage_id'] = 'COV-' + cid
    return c


def load_claims(path=None, limit=None):
    path = Path(path) if path else ROOT / 'data' / 'stress' / 'claims.jsonl'
    rows = [json.loads(line) for line in path.read_text(encoding='utf-8').splitlines() if line.strip()]
    return rows[:limit] if limit else rows


def host_facts():
    """What the report prints in its 'machine' table. Anything missing is recorded as null, never guessed."""
    def first(cmd):
        code, out = sh(cmd)
        return out if code == 0 and out else None
    mem = None
    try:
        for line in Path('/proc/meminfo').read_text().splitlines():
            if line.startswith('MemTotal:'):
                mem = int(line.split()[1]) * 1024
    except OSError:
        pass
    return {
        'collected': now_iso(), 'hostname': platform.node(), 'platform': platform.platform(), 'python': sys.version.split()[0],
        'cpu_count': os.cpu_count(), 'cpu_model': first("lscpu | sed -n 's/^Model name:\\s*//p'") if shutil.which('lscpu') else platform.processor() or None,
        'mem_total_bytes': mem,
        'gpu': first('nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader') if shutil.which('nvidia-smi') else None,
        'docker': first('docker --version'), 'compose': first('docker compose version --short'),
        'git_commit': first(f'git -C "{ROOT}" rev-parse --short HEAD'),
    }
