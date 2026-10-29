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


def _offer_demo_logins(app, accounts, feed=None):
    """Demo mode only (never registered otherwise): GET /demo/login/{level} hands the sign-in page a generated demo login, as the desktop
    app's bridge does. Answers only requests from this PC, and the demo itself refuses any non-local bind."""
    import pyotp
    from fastapi import Request
    from fastapi.responses import JSONResponse

    def local(request):
        return (request.client.host if request.client else None) in ('127.0.0.1', '::1')

    def demo_login(level: int, request: Request, badge: str = ''):
        if not local(request):
            return JSONResponse({'error': 'not found'}, status_code=404)
        for b, a in accounts.items():
            if a['level'] == level and (not badge or b == badge):
                return JSONResponse({'badge': b, 'password': a['password'], 'code': pyotp.TOTP(a['secret']).now()},
                                    headers={'Cache-Control': 'no-store'})
        return JSONResponse({'error': 'not found'}, status_code=404)

    # The demo staff for the sign-in page's employee cards: who exists at which clearance level. No password, no seed.
    def demo_people(request: Request):
        if not local(request):
            return JSONResponse({'error': 'not found'}, status_code=404)
        return JSONResponse([{'badge': b, 'name': a['name'], 'level': a['level'], 'title': a.get('title', '')} for b, a in accounts.items()],
                            headers={'Cache-Control': 'no-store'})

    # The notification bell's source: how many claims entered intake since the demo started, and the latest ones (synthetic ids only).
    def demo_feed(request: Request):
        if not local(request) or feed is None:
            return JSONResponse({'error': 'not found'}, status_code=404)
        return JSONResponse(feed.snapshot(), headers={'Cache-Control': 'no-store'})

    # "Simulate new claims": sends 1 to 5 more sample claims through the real intake. The custom header cannot be sent by another
    # site's form or image, so a web page elsewhere cannot trigger it through the browser (a cross-site fetch would need CORS).
    def demo_arrivals(request: Request, n: int = 1):
        if not local(request) or feed is None:
            return JSONResponse({'error': 'not found'}, status_code=404)
        if request.headers.get('x-cg-demo') != '1' or not 1 <= n <= 5:
            return JSONResponse({'error': 'invalid_request'}, status_code=400)
        return JSONResponse({'accepted': feed.arrive(n), 'left': len(feed.pool)}, headers={'Cache-Control': 'no-store'})

    app.add_api_route('/demo/login/{level}', demo_login, methods=['GET'], include_in_schema=False)
    app.add_api_route('/demo/people', demo_people, methods=['GET'], include_in_schema=False)
    app.add_api_route('/demo/feed', demo_feed, methods=['GET'], include_in_schema=False)
    app.add_api_route('/demo/arrivals', demo_arrivals, methods=['POST'], include_in_schema=False)


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
    parser.add_argument('--behind-proxy', action='store_true',
                        help='listen without TLS because a TLS-terminating proxy in front is the only thing that can reach this port; '
                             'client addresses are then read from X-Forwarded-For (only from TRUSTED_PROXY_IPS, default: any sender, so keep the port private)')
    parser.add_argument('--ssl-keyfile')
    parser.add_argument('--ssl-certfile')
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    if args.demo:
        args.dev = args.queue = True
        env.setdefault('COOKIE_SECURE', 'false')           # the demo is plain http on this PC
    if args.dev and args.host not in LOCAL:
        say('error: dev mode is only allowed on localhost')
        return 2
    if args.behind_proxy and args.dev:
        say('error: --behind-proxy is for real deployments, not dev mode')
        return 2
    if args.host not in LOCAL and not (args.ssl_keyfile and args.ssl_certfile) and not args.behind_proxy:
        say('error: refusing to listen on a non-local address without TLS (give --ssl-keyfile and --ssl-certfile)')
        return 2
    try:
        app, stack = bootstrap.build_app(env, dev=args.dev, store=store, data_dir=data_dir, claims_path=claims_path, bind_host=args.host,
                                       queue=args.queue, demo=args.demo)
    except config.ConfigError as e:
        say(f'configuration error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
    if args.demo:
        from workqueue import demo
        from engine_core import load_jsonl
        sample = load_jsonl(ROOT / 'data' / 'development' / 'claims.jsonl')
        accounts, loop = demo.start_demo(stack, sample)
        feed = demo.ArrivalFeed(stack.queue, sample[60:-demo.HISTORY_DONORS])    # the claims not loaded at start (nor used as earlier visits) feed "Simulate new claims"
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
        _offer_demo_logins(app, accounts, feed)
        say('Demo sign-in: open the console and press a role button; no password or code to type.')
    if args.dev:
        where = 'in the configured database' if stack.settings.mongo_uri else 'in memory (lost on exit)'
        say(f'DEV MODE: users live {where}; generated secrets are kept in {stack.data_dir / "dev_secrets.json"}. Do not use real data.')
    say(f'listening on {args.host}:{args.port}')
    if run is None:
        import uvicorn
        run = uvicorn.run
    proxy = {'proxy_headers': True, 'forwarded_allow_ips': (env.get('TRUSTED_PROXY_IPS') or '*').strip()} if args.behind_proxy else {}
    run(app, host=args.host, port=args.port, server_header=False, ssl_keyfile=args.ssl_keyfile, ssl_certfile=args.ssl_certfile, **proxy)
    return 0


if __name__ == '__main__':
    sys.exit(main())
