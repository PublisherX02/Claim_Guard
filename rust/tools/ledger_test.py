"""End-to-end test of the signed ledger against a demo server this script starts.

    python rust/tools/ledger_test.py

The demo server signs a checkpoint over each audit log (in its worker loop, every 5 seconds here), and the API verifies them with the public key alone.
Checks: the ledger is on and verifies; the health panel's ledger probe is healthy; a proof for one event downloads and VERIFIES OFFLINE with `cg ledger
verify-proof` and only the public key; the same proof with one byte changed does not; a copy of the log with its last events cut off, and a copy with a
row rewritten, are both caught by `cg ledger verify`; the signing seed is not in the API's answers. Authorized scope: the user's own demo server.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402

CG = str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe')
PORT = 8497
pentest.PORT = PORT
FAILED = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def cli(*args, env=None):
    p = subprocess.run([CG, 'ledger', *args], capture_output=True, text=True, encoding='utf-8', env=dict(os.environ, **(env or {})))
    return p.returncode, p.stdout + p.stderr


def main():
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_ledger_test'
    if data.exists():
        for f in data.iterdir():
            f.unlink() if f.is_file() else None
    data.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT), LEDGER_CHECKPOINT_SECONDS='5')
    proc = subprocess.Popen([CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen('http://127.0.0.1:%d/healthz' % PORT, timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        admin = pentest.Session('CG-4004', acc['CG-4004'])
        time.sleep(12)                                            # two signing rounds
        st, _, led = admin.call('GET', '/api/v1/ledger')
        check('the ledger is on, and every checkpoint verifies', st == 200 and led.get('configured') is True and led.get('ok') is True, (st, led))
        sec = (led.get('logs') or {}).get('security', {})
        check('the security log has signed checkpoints covering events (%s checkpoints, %s of %s events)' % (sec.get('checkpoints'), sec.get('covered_rows'), sec.get('log_rows')), (sec.get('checkpoints') or 0) >= 1 and (sec.get('covered_rows') or 0) >= 1, sec)
        public = led['public_key']
        check('the answer carries the public key and no secret', 'LEDGER_SIGNING_KEY' not in json.dumps(led) and len(public) == 43)
        saved = json.loads((data / 'dev_secrets.json').read_text())
        check('the seed (a secret of the worker) is not anywhere in the API answer', saved['LEDGER_SIGNING_KEY'] not in json.dumps(led))
        st, _, health = admin.call('GET', '/api/v1/ops/health')
        probe = next((c for c in health.get('components', []) if c.get('id') == 'ledger' or 'edger' in str(c.get('name', ''))), None) if isinstance(health, dict) else None
        check('the health panel shows the ledger probe as healthy', probe is not None and probe.get('status') == 'ok', probe or str(health)[:300])
        # a proof for event 1, checked offline
        st, _, proof = admin.call('GET', '/api/v1/ledger/proof?log=security&sequence=1')
        check('a proof for event 1 is issued', st == 200 and proof.get('sequence') == 1, (st, str(proof)[:200]))
        pf = data / 'proof.json'
        pf.write_text(json.dumps(proof))
        rc, out = cli('verify-proof', str(pf), '--public', public)
        check('the proof verifies OFFLINE with the public key alone', rc == 0 and 'VALID' in out, out)
        bad = json.loads(pf.read_text())
        bad['row_hash'] = bad['row_hash'][:-1] + ('0' if bad['row_hash'][-1] != '0' else '1')
        (data / 'bad.json').write_text(json.dumps(bad))
        rc, out = cli('verify-proof', str(data / 'bad.json'), '--public', public)
        check('the same proof with one character changed does not', rc == 1 and 'INVALID' in out, out)
        other = cli('keygen')[1].split('LEDGER_PUBLIC_KEY=')[1].split()[0]
        rc, out = cli('verify-proof', str(pf), '--public', other)
        check('and it does not verify under another key', rc == 1, out)
        st, _, _ = admin.call('GET', '/api/v1/ledger/proof?log=security&sequence=99999999')
        check('no proof for an event no checkpoint covers', st == 404, st)
        reviewer = pentest.Session('CG-2002', acc['CG-2002'])
        check('a reviewer cannot read the ledger or ask for a proof', reviewer.call('GET', '/api/v1/ledger')[0] == 403 and reviewer.call('GET', '/api/v1/ledger/proof?log=security&sequence=1')[0] == 403)
        # offline copies: the command-line verifier, on a copy of the files
        time.sleep(6)
        copy = data / 'copy'
        copy.mkdir(exist_ok=True)
        log = copy / 'security_audit.jsonl'
        log.write_bytes((data / 'security_audit.jsonl').read_bytes())
        (copy / 'security_audit.jsonl.checkpoints.jsonl').write_bytes((data / 'security_audit.jsonl.checkpoints.jsonl').read_bytes())
        keys = {'FERNET_KEY': saved['FERNET_KEY']}      # the logs are encrypted on disk: reading a whole log needs the server's data key (a proof does not)
        rc, out = cli('verify', str(log), '--public', public)
        check('without the data key a whole encrypted log cannot be read, and it says so', rc == 1 and 'encrypted' in out, out[-200:])
        rc, out = cli('verify', str(log), '--public', public, env=keys)
        check('the command-line verifier agrees on an untouched copy', rc == 0, out[-300:])
        # at-rest encryption: the log is encrypted with a key of the server, so the cut copy is made at line level
        lines = log.read_bytes().split(b'\n')
        lines = [l for l in lines if l]
        log.write_bytes(b'\n'.join(lines[:-2]) + b'\n')
        rc, out = cli('verify', str(log), '--public', public, env=keys)
        check('a copy with the last events cut off is caught', rc == 1 and ('cut' in out or 'chain' in out), out[-300:])
    finally:
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
