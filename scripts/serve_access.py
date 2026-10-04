"""Run the reviewer API.

    python scripts/serve_access.py                      # production: secrets and MONGO_URI from the environment
    python scripts/serve_access.py --dev                # local demo: generated secrets, in-memory users, localhost only

It binds to 127.0.0.1 by default. Binding anywhere else requires TLS (--ssl-keyfile and --ssl-certfile), because the session
cookie is marked Secure and passwords cross the wire; dev mode never leaves localhost.
"""
import argparse
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
    parser.add_argument('--ssl-keyfile')
    parser.add_argument('--ssl-certfile')
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    if args.dev and args.host not in LOCAL:
        say('error: dev mode is only allowed on localhost')
        return 2
    if args.host not in LOCAL and not (args.ssl_keyfile and args.ssl_certfile):
        say('error: refusing to listen on a non-local address without TLS (give --ssl-keyfile and --ssl-certfile)')
        return 2
    try:
        app, stack = bootstrap.build_app(env, dev=args.dev, store=store, data_dir=data_dir, claims_path=claims_path, bind_host=args.host)
    except config.ConfigError as e:
        say(f'configuration error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
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
