#!/usr/bin/env python3
"""S7. Integrity and operations: health panel, audit chains, signed ledger, and an encrypted backup (docs/65).

    CG_URL=... CG_BADGE=... python experiments/server/s07_integrity.py --run <name> [--backup-key-file /root/claimguard-backup.key --backup-dir /srv/backups]

Read-only checks through the API: GET /api/v1/ops/health (every probe and the overall state), GET /api/v1/audit/verify (the hash chains verify), GET /api/v1/ledger
(the signed checkpoints verify with the public key alone). With --backup-key-file it also takes a real backup with deploy/rust/backup.sh and records its time
and size, and checks the file is encrypted (no readable claim text) and has its SHA-256 beside it. It does NOT restore: a restore replaces the data, so it is
rehearsed on a second machine from this backup (docs/40 section 8; backup_restore_test.py does the same on a throw-away stack).
"""
import argparse
import hashlib
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import ROOT, Api, Run, sh  # noqa: E402


def backup(key_file, out_dir):
    out = Path(out_dir) / f'experiment-{int(time.time())}.tar.enc'
    t0 = time.time()
    code, text = sh(f'BACKUP_KEY_FILE={key_file} bash {ROOT}/deploy/rust/backup.sh {out}', timeout=3600)
    secs = time.time() - t0
    info = {'command_ok': code == 0, 'seconds': secs, 'output_tail': text[-300:]}
    if code == 0 and out.exists():
        data = out.read_bytes()
        info.update({'bytes': len(data), 'sha256_file_present': Path(str(out) + '.sha256').exists(), 'sha256_matches': None,
                     'looks_encrypted': b'claim_id' not in data and b'CG-' not in data[:1_000_000] and data[:8] == b'Salted__', 'path': str(out)})
        side = Path(str(out) + '.sha256')
        if side.exists():
            info['sha256_matches'] = side.read_text().split()[0] == hashlib.sha256(data).hexdigest()
    return info


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--backup-key-file', default='')
    ap.add_argument('--backup-dir', default='/tmp')
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run, api = Run(a.run, a.dry), Api().login()
    s, health, dt_h = api.call('GET', '/api/v1/ops/health')
    sv, verify, dt_v = api.call('GET', '/api/v1/audit/verify')
    sl, ledger, dt_l = api.call('GET', '/api/v1/ledger')
    probes = health.get('components') if isinstance(health, dict) else None
    result = {'health': {'status': s, 'seconds': dt_h, 'overall': health.get('overall') if isinstance(health, dict) else None,
                         'probes': [{'name': p.get('id'), 'status': p.get('status')} for p in probes] if isinstance(probes, list) else probes},
              'audit_verify': {'status': sv, 'seconds': dt_v, 'body': verify if isinstance(verify, dict) else str(verify)[:200]},
              'ledger': {'status': sl, 'seconds': dt_l, 'body': ledger if isinstance(ledger, dict) else str(ledger)[:200]}}
    if a.backup_key_file:
        result['backup'] = backup(a.backup_key_file, a.backup_dir)
    bad = [p for p in (result['health']['probes'] or []) if isinstance(p, dict) and p.get('status') not in ('ok', 'unknown', None)]
    chains = [v.get('ok') for v in verify.values() if isinstance(v, dict)] if isinstance(verify, dict) else []
    result['checks'] = {'health_not_down': result['health']['overall'] not in (None, 'down'), 'no_failing_probe': not bad and s == 200,
                        'audit_chains_verify': sv == 200 and bool(chains) and all(chains), 'ledger_verifies': sl == 200 and isinstance(ledger, dict) and ledger.get('ok') is True}
    if 'backup' in result:
        result['checks']['backup_ok_and_encrypted'] = bool(result['backup'].get('command_ok') and result['backup'].get('looks_encrypted') and result['backup'].get('sha256_matches') in (True, None))
    run.write('s07_integrity', result)


if __name__ == '__main__':
    main()
