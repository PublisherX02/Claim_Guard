"""Operate the work queue from the command line. Every subcommand needs a level 4 operator who signs in with badge, password and
authenticator code, and every use is written to the security log.

    python scripts/queue_admin.py --badge CG-4004 submit --claims data/development/claims.jsonl --process
    python scripts/queue_admin.py --badge CG-4004 reconcile
    python scripts/queue_admin.py --badge CG-4004 replay CG-27BFD8541DEB
    python scripts/queue_admin.py --badge CG-4004 rerun --rule-pack <old pack hash> [--apply]
    python scripts/queue_admin.py --badge CG-4004 deadletters
    python scripts/queue_admin.py --badge CG-4004 deadletter-replay <dead id>
    python scripts/queue_admin.py --badge CG-4004 verify-deal <deal id>

The password and the code are asked for without echo and are never printed or logged. Read-only commands need queue.view; the ones
that change the queue (submit, rerun --apply, deadletter-replay) need routing.manage. Nothing here decides a claim: level 4 cannot.
Secrets and MONGO_URI come from the environment like the server's; --dev uses generated secrets and an in-memory queue for a demo.
"""
import argparse
import getpass
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import bootstrap as access_bootstrap, config  # noqa: E402
from access.service import AuthError, Forbidden  # noqa: E402
from access.store import StoreUnavailable  # noqa: E402
from workqueue import bootstrap, pipeline, reconcile, replay  # noqa: E402
from workqueue.service import Conflict, NotFound  # noqa: E402


def main(argv=None, env=None, getpass_fn=getpass.getpass, out=None, store=None, queue_store=None, data_dir=None, engine=None,
         clock=None):
    out = out or sys.stdout
    env = dict(os.environ if env is None else env)

    def say(text=''):
        print(text, file=out)

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--dev', action='store_true')
    parser.add_argument('--badge', required=True, help='the operator\'s badge, e.g. CG-4004')
    sub = parser.add_subparsers(dest='command', required=True)
    r = sub.add_parser('replay')
    r.add_argument('claim_id')
    r.add_argument('--version', type=int)
    rr = sub.add_parser('rerun')
    rr.add_argument('--rule-pack', required=True)
    rr.add_argument('--apply', action='store_true')
    sub.add_parser('deadletters')
    sub.add_parser('deadletter-replay').add_argument('dead_id')
    sub.add_parser('reconcile')
    sub.add_parser('verify-deal').add_argument('deal_id')
    s = sub.add_parser('submit')
    s.add_argument('--claims', required=True)
    s.add_argument('--process', action='store_true', help='also run each claim through the pipeline up to ready (no broker needed)')
    try:
        args = parser.parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    try:
        stack = access_bootstrap.build_stack(env, dev=args.dev, store=store, data_dir=data_dir, clock=clock)
        queue = bootstrap.build_queue(stack, env, dev=args.dev, queue_store=queue_store, engine=engine, **({'clock': clock} if clock else {}))
    except config.ConfigError as e:
        say(f'configuration error: {e}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2

    # ---- sign in as the operator
    try:
        password, code = getpass_fn('Password: '), getpass_fn('Authenticator code: ')
        session = stack.service.login(args.badge, password, code, client='queue_admin')
        operator = stack.service.authenticate(session.token)
    except AuthError:
        say('error: sign-in failed')
        return 2
    mutating = args.command in ('submit', 'deadletter-replay') or (args.command == 'rerun' and args.apply)
    needed = 'routing.manage' if mutating else 'queue.view'
    try:
        stack.service.require(operator, needed)
        stack.service.record('queue_admin', actor=operator.badge, command=args.command)
    except Forbidden:
        say('error: this operator may not do that')
        return 2
    except AuthError:
        say('error: the security log cannot be written; nothing was done')
        return 2

    store_q, now = queue.store, queue.service.clock
    try:
        code_out = _run(args, queue, store_q, now, operator, say)
    except (NotFound, KeyError):
        say('error: not found')
        return 2
    except Conflict as e:
        say(f'error: conflict ({e})')
        return 2
    except (ValueError, OSError) as e:
        say(f'error: {str(e)[:200]}')
        return 2
    except StoreUnavailable:
        say('error: the database cannot be reached')
        return 2
    return code_out


def _run(args, queue, store, clock, operator, say):
    if args.command == 'replay':
        out = replay.replay_claim(store, queue.engine, args.claim_id, args.version)
        say(json.dumps(out, sort_keys=True))
        return 0 if out['same'] and out['claim_intact'] and out['results_intact'] else 1
    if args.command == 'rerun':
        out = replay.rerun(store, queue.engine, args.rule_pack, queue.intake, dry_run=not args.apply)
        say(json.dumps(out, sort_keys=True))
        return 0
    if args.command == 'deadletters':
        for d in store.dead_letters(1000):
            say(f"{d['dead_id']}  {d['claim_id']} v{d['version']}  {d['reason']}  attempts={d['attempts']}")
        return 0
    if args.command == 'deadletter-replay':
        moved = replay.replay_dead_letter(store, args.dead_id, clock(), operator.badge)
        say(f"{moved['claim_id']} v{moved['version']} is back in {moved['state']}")
        return 0
    if args.command == 'reconcile':
        report = reconcile.reconcile(store, clock())
        for f in report.findings:
            say(json.dumps(f, sort_keys=True))
        say(f'ok={report.ok} findings={len(report.findings)}')
        return 0 if report.ok else 1
    if args.command == 'verify-deal':
        same = queue.dispatcher.verify(args.deal_id)
        say(f'deal {args.deal_id}: {"matches the algorithm" if same else "DOES NOT match the algorithm (the stored seed or inputs were changed)"}')
        return 0 if same else 1
    if args.command == 'submit':
        from engine_core import load_jsonl
        submitted = refused = ready = 0
        for claim in load_jsonl(args.claims):
            try:
                queue.intake.submit(claim)
            except ValueError:
                refused += 1
                continue
            submitted += 1
            if args.process:
                doc = store.get(claim['claim_id'])
                if pipeline.advance(store, doc['claim_id'], doc['version'], queue.runtime.steps, clock()) == 'ready':
                    store.clear_outbox(doc['claim_id'], doc['version'])
                    ready += 1
        say(f'submitted={submitted} refused={refused} ready={ready}')
        return 0
    return 2


if __name__ == '__main__':
    sys.exit(main())
