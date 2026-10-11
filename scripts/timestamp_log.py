"""Timestamp the audit log's anchor with an RFC 3161 Time-Stamp Authority, or check an existing timestamp.

    python scripts/timestamp_log.py stamp  --log outputs/audit/audit.jsonl --tsa-url https://tsa.example/tsr
    python scripts/timestamp_log.py verify --log outputs/audit/audit.jsonl --tsa-ca tsa_ca.pem

Run `stamp` on a schedule (for example every hour, or after each batch). It never changes the log. If the TSA is unreachable it
prints the reason and exits 2, and the previous token stays in place. `verify` prints one of: none, unverified, current, stale,
invalid (src/timestamp_anchor.py explains each) and exits 1 only for 'invalid'.
"""
import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import timestamp_anchor as ta


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest='cmd', required=True)
    s = sub.add_parser('stamp')
    s.add_argument('--log', required=True)
    s.add_argument('--tsa-url', required=True)
    s.add_argument('--timeout', type=float, default=10)
    v = sub.add_parser('verify')
    v.add_argument('--log', required=True)
    v.add_argument('--tsa-ca', help='PEM file with the CA certificate(s) allowed to issue the TSA certificate')
    a = p.parse_args(argv)
    anchor = Path(a.log).with_name(Path(a.log).name + '.head.json')
    if a.cmd == 'stamp':
        try:
            side = ta.stamp_log(a.log, anchor, a.tsa_url, timeout=a.timeout)
        except ta.TimestampError as exc:
            print(f'NOT timestamped: {exc}. The log is unaffected; the previous token (if any) is kept.')
            return 2
        print(f'Stamped head {side["head"][:16]}... at {side["count"]} events; token saved to {ta.sidecar_path(a.log)}.')
        print('Run `verify` with the TSA CA certificate to check it; stamping does not verify the token.')
        return 0
    ca = Path(a.tsa_ca).read_bytes() if a.tsa_ca else None
    st = ta.timestamp_status(a.log, anchor, ca)
    print('Timestamp:', json.dumps(st))
    return 1 if st['state'] == 'invalid' else 0


if __name__ == '__main__':
    sys.exit(main())
