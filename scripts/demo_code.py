"""Print the current authenticator code of a demo account (written by `serve_access.py --demo` to instance/demo_accounts.json).
For local demos only: a real account's authenticator seed is never stored in the clear."""
import json
import sys
from pathlib import Path

import pyotp

ROOT = Path(__file__).resolve().parents[1]


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    path = ROOT / 'instance' / 'demo_accounts.json'
    if len(argv) != 1 or not path.exists():
        print('usage: python scripts/demo_code.py CG-2002   (start the server with --demo first)')
        return 2
    account = json.loads(path.read_text(encoding='utf-8')).get(argv[0])
    if account is None:
        print('no such demo account')
        return 2
    print(pyotp.TOTP(account['secret']).now())
    return 0


if __name__ == '__main__':
    sys.exit(main())
