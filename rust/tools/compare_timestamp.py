"""RFC 3161 across the two builds, over a real HTTP exchange with a throw-away Time-Stamp Authority (tests/local_tsa.py).

    python rust/tools/compare_timestamp.py

1. The Rust tool stamps a log that Python wrote; the Python verifier calls the stamp current; the Rust verifier agrees.
2. The Python tool stamps; the Rust verifier agrees; after new events both call it stale with the same count.
3. A log rewritten after stamping is invalid for both.
4. An authority that is down: Rust exits 2 and keeps the previous token.
"""
import json, os, subprocess, sys, tempfile, threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import timestamp_anchor as ta  # noqa: E402
from audit_log import ANCHOR_KEY_ENV, AuditLog  # noqa: E402
from local_tsa import LocalTSA  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
EVENTS = [{'event_type': 'run_started', 'run_id': 'run-1', 'claim_id': 'CG-1', 'input_hash': 'h'},
          {'event_type': 'rule_check', 'run_id': 'run-1', 'claim_id': 'CG-1', 'rule_id': 'R001', 'status': 'PASS'},
          {'event_type': 'run_finished', 'run_id': 'run-1', 'claim_id': 'CG-1'}]
RESULTS = []


def check(name, ok, detail=''):
    RESULTS.append(ok)
    print(('PASS ' if ok else 'FAIL ') + name + (f'  -- {detail}' if detail and not ok else ''))


def main():
    tsa = LocalTSA()
    up = {'on': True}

    class H(BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers['Content-Length']))
            if not up['on']:
                self.send_response(503)
                self.end_headers()
                return
            out = tsa.respond(body)
            self.send_response(200)
            self.send_header('Content-Type', 'application/timestamp-reply')
            self.send_header('Content-Length', str(len(out)))
            self.end_headers()
            self.wfile.write(out)

        def log_message(self, *a):
            pass

    srv = HTTPServer(('127.0.0.1', 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{srv.server_port}/tsr'
    tmp = Path(tempfile.mkdtemp())
    ca = tmp / 'ca.pem'
    ca.write_bytes(tsa.trusted_pem)
    with mock.patch.dict(os.environ):
        os.environ.pop(ANCHOR_KEY_ENV, None)
        log_path = tmp / 'audit.jsonl'
        log = AuditLog(log_path)
        log._write(EVENTS)
    anchor = log.anchor_path

    def rust(*a):
        p = subprocess.run([CG, 'timestamp', *a], capture_output=True, text=True, encoding='utf-8')
        return p.returncode, p.stdout

    rc, out = rust('stamp', '--log', str(log_path), '--tsa-url', url)
    check('Rust stamps over HTTP', rc == 0, out)
    st = ta.timestamp_status(log_path, anchor, tsa.trusted_pem)
    check('Python calls the Rust stamp current', st['state'] == 'current', st)
    rc, out = rust('verify', '--log', str(log_path), '--tsa-ca', str(ca))
    check('Rust verifies its own stamp as current', rc == 0 and '"current"' in out, out)
    rc, out = rust('verify', '--log', str(log_path))
    check('without a CA the stamp is unverified, not current', '"unverified"' in out, out)
    side = ta.stamp_log(log_path, anchor, url, opener=None)
    check('Python stamps over HTTP', side['count'] == 3)
    rc, out = rust('verify', '--log', str(log_path), '--tsa-ca', str(ca))
    check('Rust verifies the Python stamp as current', rc == 0 and '"current"' in out, out)
    with mock.patch.dict(os.environ):
        os.environ.pop(ANCHOR_KEY_ENV, None)
        AuditLog(log_path)._write([{'event_type': 'run_started', 'run_id': 'run-2', 'claim_id': 'CG-2', 'input_hash': 'h2'}])
    py, (rc, out) = ta.timestamp_status(log_path, anchor, tsa.trusted_pem), rust('verify', '--log', str(log_path), '--tsa-ca', str(ca))
    rs = json.loads(out.split('Timestamp: ', 1)[1])
    check('both call the stamp stale with the same count of unstamped events', py['state'] == rs['state'] == 'stale' and py['unstamped_events'] == rs['unstamped_events'] == 1, (py, rs))
    check('both report the same signer, time and serial', all(py[k] == rs[k] for k in ('gen_time', 'tsa_subject', 'serial')), (py, rs))
    lines = log_path.read_text(encoding='utf-8').splitlines()
    row = json.loads(lines[2])
    row['hash'] = '00' * 32
    lines[2] = json.dumps(row)
    log_path.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    py, (rc, out) = ta.timestamp_status(log_path, anchor, tsa.trusted_pem), rust('verify', '--log', str(log_path), '--tsa-ca', str(ca))
    check('a rewritten log is invalid for both (Rust exits 1)', py['state'] == 'invalid' and rc == 1 and '"invalid"' in out, (py, rc, out))
    before = ta.sidecar_path(log_path).read_text(encoding='utf-8')
    up['on'] = False
    rc, out = rust('stamp', '--log', str(log_path), '--tsa-url', url)
    check('an authority that is down: exit 2, log untouched, previous token kept', rc == 2 and ta.sidecar_path(log_path).read_text(encoding='utf-8') == before, (rc, out))
    print(f'\n{len(RESULTS)} checks, {RESULTS.count(False)} failed')
    return 0 if all(RESULTS) else 1


if __name__ == '__main__':
    sys.exit(main())
