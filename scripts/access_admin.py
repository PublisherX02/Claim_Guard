"""Administer reviewer accounts from the command line.

    python scripts/access_admin.py create-admin --badge CG-0001 --name "Ada Admin"
    python scripts/access_admin.py list
    python scripts/access_admin.py unlock --badge CG-1234
    python scripts/access_admin.py reset-totp --badge CG-1234
    python scripts/access_admin.py --dev seed-demo        # demo accounts, one per level (dev mode only)

Secrets come from the environment (see .env.example); --dev generates them into the instance directory for a local demo.
This tool talks to the database directly, so it needs the same access as the server and should be run only by whoever
operates the system. A provisioning URI (which contains the authenticator seed) and a generated demo password are printed once
and never again; a password you type is never printed.
"""
import argparse
import getpass
import os
import secrets
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import bootstrap, config, passwords, totp  # noqa: E402
from access.service import DuplicateUser  # noqa: E402
from access.store import StoreUnavailable  # noqa: E402

DEMO_USERS = ((1, 'CG-1001', 'Demo Viewer'), (2, 'CG-2002', 'Demo Reviewer'), (3, 'CG-3003', 'Demo Senior Reviewer'),
              (4, 'CG-4004', 'Demo Administrator'))


def _demo_password(badge):
    while True:
        candidate = secrets.token_urlsafe(14) + 'aA1!'
        if not passwords.check_policy(candidate, badge):
            return candidate


def main(argv=None, env=None, getpass_fn=getpass.getpass, out=None, store=None, data_dir=None):
    out = out or sys.stdout
    env = dict(os.environ if env is None else env)

    def say(text=''):
        print(text, file=out)

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--dev', action='store_true', help='local demo mode: generate secrets, allow cheap hashing and seed-demo')
    sub = parser.add_subparsers(dest='command', required=True)
    create = sub.add_parser('create-admin', help='create a level 4 administrator')
    create.add_argument('--badge', required=True)
    create.add_argument('--name', required=True)
    sub.add_parser('seed-demo', help='create one demo user per level (dev mode only)')
    sub.add_parser('list', help='list accounts')
    for name in ('unlock', 'reset-totp'):
        sub.add_parser(name).add_argument('--badge', required=True)
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    if args.command == 'seed-demo' and not args.dev:
        say('error: seed-demo is only allowed with --dev')
        return 2
    try:
        stack = bootstrap.build_stack(env, dev=args.dev, store=store, data_dir=data_dir)
    except config.ConfigError as e:
        say(f'configuration error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
    svc, db, log = stack.service, stack.store, stack.securitylog

    try:
        if args.command == 'create-admin':
            first, again = getpass_fn('New password: '), getpass_fn('Repeat password: ')
            if first != again:
                say('error: the passwords do not match')
                return 2
            _, uri = svc.provision_user(args.badge, args.name, first, 4, created_by='cli', must_change_password=False)
            log.record('user_created', actor='cli', badge_id=args.badge, level=4)
            say(f'created {args.badge} (administrator)')
            say('Add this to an authenticator app now; it is shown once:')
            say(uri)
        elif args.command == 'seed-demo':
            if any(db.get_user(badge) for _, badge, _ in DEMO_USERS):
                say('error: a demo account already exists; nothing was created')
                return 2
            for level, badge, name in DEMO_USERS:
                password = _demo_password(badge)
                _, uri = svc.provision_user(badge, name, password, level, created_by='cli', must_change_password=False)
                log.record('user_created', actor='cli', badge_id=badge, level=level)
                say(f'{badge} level {level} {name} password: {password}')
                say(uri)
        elif args.command == 'list':
            for u in sorted(db.list_users(), key=lambda u: u.badge_id):
                locked = ' locked' if u.locked_until else ''
                say(f'{u.badge_id}  level {u.level}  {"active" if u.active else "inactive"}{locked}  {u.name}')
        else:
            user = db.get_user(args.badge) if isinstance(args.badge, str) else None
            if user is None:
                say('error: no such user')
                return 2
            if args.command == 'unlock':
                db.update_user(args.badge, failed_attempts=0, locked_until=None)
                log.record('user_unlocked', actor='cli', badge_id=args.badge)
                say(f'unlocked {args.badge}')
            else:
                secret = totp.new_secret()
                db.update_user(args.badge, totp_secret_enc=totp.encrypt_secret(secret, stack.settings.fernet_key))
                log.record('totp_reset', actor='cli', badge_id=args.badge)
                say('Add this to an authenticator app now; it is shown once:')
                say(totp.provisioning_uri(secret, args.badge))
    except DuplicateUser:
        say('error: that badge already exists')
        return 2
    except ValueError as e:
        say(f'error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
