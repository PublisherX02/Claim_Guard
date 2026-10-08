"""Run the reviewer API.

    python scripts/serve_access.py                      # production: secrets and MONGO_URI from the environment
    python scripts/serve_access.py --dev                # local demo: generated secrets, in-memory users, localhost only
    python scripts/serve_access.py --demo               # everything for a demo: accounts, sample claims, in-process workers, the console

The reviewer console is served at / (open the printed address in a browser). Behind a TLS proxy it is the same page.

It binds to 127.0.0.1 by default. Binding anywhere else requires TLS (--ssl-keyfile and --ssl-certfile), because the session
cookie is marked Secure and passwords cross the wire; dev mode never leaves localhost.
"""
import argparse
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import bootstrap, config  # noqa: E402
from access.store import StoreUnavailable  # noqa: E402

LOCAL = config.LOCAL_HOSTS


def main(argv=None, env=None, out=None, run=None, store=None, data_dir=None, claims_path=None):
    out = out or sys.stdout
    env = dict(os.environ if env is None else env)

    def say(text):
        print(text, file=out)

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, default=8443)
    parser.add_argument('--dev', action='store_true')
    parser.add_argument('--demo', action='store_true', help='implies --dev --queue; also seeds demo accounts and claims and runs the workers in-process')
    parser.add_argument('--queue', action='store_true', help='also serve the work queue routes (needs the queue database)')
    parser.add_argument('--ssl-keyfile')
    parser.add_argument('--ssl-certfile')
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    if args.demo:
        args.dev = args.queue = True
    if args.dev and args.host not in LOCAL:
        say('error: dev mode is only allowed on localhost')
        return 2
    if args.host not in LOCAL and not (args.ssl_keyfile and args.ssl_certfile):
        say('error: refusing to listen on a non-local address without TLS (give --ssl-keyfile and --ssl-certfile)')
        return 2
    try:
        app, stack = bootstrap.build_app(env, dev=args.dev, store=store, data_dir=data_dir, claims_path=claims_path, bind_host=args.host,
                                       queue=args.queue)
    except config.ConfigError as e:
        say(f'configuration error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
    if args.demo:
        from workqueue import demo
        from engine_core import load_jsonl
        accounts, loop = demo.start_demo(stack, load_jsonl(ROOT / 'data' / 'development' / 'claims.jsonl'))
        path = stack.data_dir / 'demo_accounts.json'
        path.write_text(json.dumps(accounts, indent=1), encoding='utf-8')
        try:
            os.chmod(path, 0o600)
        except OSError:
            pass
        say('DEMO ACCOUNTS (generated now, kept in memory and in ' + str(path) + '; never use real data):')
        for badge, a in accounts.items():
            say(f'  {badge}  level {a["level"]}  {a["name"]:<22} password: {a["password"]}')
        say('Authenticator codes: python scripts/demo_code.py CG-2002   (prints the current 6-digit code)')
    if args.dev:
        where = 'in the configured database' if stack.settings.mongo_uri else 'in memory (lost on exit)'
        say(f'DEV MODE: users live {where}; generated secrets are kept in {stack.data_dir / "dev_secrets.json"}. Do not use real data.')
    say(f'listening on {args.host}:{args.port}')
    if run is None:
        import uvicorn
        run = uvicorn.run
    run(app, host=args.host, port=args.port, server_header=False, ssl_keyfile=args.ssl_keyfile, ssl_certfile=args.ssl_certfile)
    return 0


if __name__ == '__main__':
    sys.exit(main())
