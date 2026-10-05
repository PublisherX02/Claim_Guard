"""scripts/queue_admin.py: it needs a real level 4 operator, writes every use to the security log and never prints a secret."""
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'tests'))
import pyotp
import queue_admin
from access import bootstrap
from audit_log import ANCHOR_KEY_ENV
from engine_core import load_jsonl
from workqueue.dispatcher import Agent, Dispatcher
from workqueue.intake import Intake
from workqueue.store import MemoryQueueStore
from wq_world import FakeEngine, set_cfg

ENV = {'BCRYPT_ROUNDS': '4'}
PASSWORDS = {4: 'Admin-Pass-4680!', 2: 'Reviewer-Pass-2468!'}
L3 = frozenset({'claims.decide', 'claims.decide_high'})


class Cli:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = mock.patch.dict(os.environ, {}, clear=False)
        self.env.start()
        os.environ.pop(ANCHOR_KEY_ENV, None)
        self.data = Path(self.tmp.name)
        self.users = __import__('access.store', fromlist=['MemoryStore']).MemoryStore()
        self.queue = MemoryQueueStore()
        self.clock_now = 1_700_000_000.0
        self.stack = bootstrap.build_stack(ENV, dev=True, store=self.users, data_dir=self.data, clock=lambda: self.clock_now)
        self.secrets = {}
        self.engine = FakeEngine()

    def close(self):
        self.env.stop()
        self.tmp.cleanup()

    def provision(self, level, badge):
        _, uri = self.stack.service.provision_user(badge, f'User {badge}', PASSWORDS[level], level, must_change_password=False)
        self.secrets[badge] = parse_qs(urlparse(uri).query)['secret'][0]

    def run(self, *argv, badge='CG-4004', password=None, code=None):
        """Run the tool as this operator; returns (exit code, everything printed)."""
        level = self.users.get_user(badge).level if self.users.get_user(badge) else 4
        self.clock_now += 31                                   # a fresh authenticator step, so a code is never replayed
        typed = [password if password is not None else PASSWORDS[level],
                 code if code is not None else pyotp.TOTP(self.secrets.get(badge, 'A' * 32)).at(self.clock_now)]
        out = io.StringIO()
        status = queue_admin.main(['--dev', '--badge', badge, *argv], env=dict(ENV), getpass_fn=lambda prompt: typed.pop(0), out=out,
                                  store=self.users, queue_store=self.queue, data_dir=self.data, engine=self.engine,
                                  clock=lambda: self.clock_now)
        return status, out.getvalue()

    def security_events(self, kind):
        log = self.stack.securitylog
        return [r['event'] for r in log.events(limit=1000) if r['event']['event_type'] == kind]


def claims_file(cli, n=6):
    path = cli.data / 'claims.jsonl'
    rows = load_jsonl(str(ROOT / 'data' / 'development' / 'claims.jsonl'))[:n]
    path.write_text('\n'.join(json.dumps(r) for r in rows) + '\n', encoding='utf-8')
    return path, rows


class AdminTests(unittest.TestCase):
    def setUp(self):
        self.c = Cli()
        self.c.provision(4, 'CG-4004')
        self.c.provision(2, 'CG-2002')

    def tearDown(self):
        self.c.close()

    # ---- who may run it
    def test_a_wrong_password_a_wrong_code_and_an_unknown_badge_all_fail_the_same_way_and_do_nothing(self):
        for kwargs in ({'password': 'Wrong-Pass-1357!'}, {'code': '000000'}, {'badge': 'CG-9999'}):
            status, out = self.c.run('reconcile', **kwargs)
            self.assertEqual((status, out.strip()), (2, 'error: sign-in failed'))
        self.assertEqual(self.c.security_events('queue_admin'), [])

    def test_a_level_two_reviewer_cannot_use_it(self):
        status, out = self.c.run('reconcile', badge='CG-2002')
        self.assertEqual((status, out.strip()), (2, 'error: this operator may not do that'))
        self.assertEqual(self.c.security_events('queue_admin'), [])

    def test_every_use_by_an_operator_is_logged_with_the_command(self):
        for command in (['reconcile'], ['deadletters']):
            self.c.run(*command)
        got = [(e['actor'], e['command']) for e in self.c.security_events('queue_admin')]
        self.assertEqual(got, [('CG-4004', 'reconcile'), ('CG-4004', 'deadletters')])

    def test_no_secret_is_ever_printed(self):
        status, out = self.c.run('reconcile')
        for secret in (PASSWORDS[4], self.c.secrets['CG-4004'], pyotp.TOTP(self.c.secrets['CG-4004']).now()):
            self.assertNotIn(secret, out)
        self.assertNotIn(PASSWORDS[4], self.c.stack.securitylog.path.read_text(encoding='utf-8'))

    def test_bad_arguments_exit_with_two(self):
        out = io.StringIO()
        with mock.patch('sys.stderr', io.StringIO()):
            self.assertEqual(queue_admin.main(['--dev'], env=dict(ENV), out=out), 2)
            self.assertEqual(queue_admin.main(['--dev', '--badge', 'CG-4004', 'nope'], env=dict(ENV), out=out), 2)

    # ---- submit, reconcile, replay
    def test_submit_with_process_takes_claims_to_ready_and_reconcile_is_clean(self):
        path, rows = claims_file(self.c)
        status, out = self.c.run('submit', '--claims', str(path), '--process')
        self.assertEqual((status, out.strip()), (0, 'submitted=6 refused=0 ready=6'))
        self.assertEqual(sum(self.c.queue.counts().values()), 6)
        self.assertTrue(all(k.startswith('ready|') for k in self.c.queue.counts()))
        status, out = self.c.run('reconcile')
        self.assertEqual((status, out.strip().splitlines()[-1]), (0, 'ok=True findings=0'))

    def test_submit_refuses_a_malformed_claim_and_carries_on(self):
        path, rows = claims_file(self.c, 2)
        with path.open('a', encoding='utf-8') as f:
            f.write(json.dumps({'claim_id': 'BAD'}) + '\n')
        status, out = self.c.run('submit', '--claims', str(path))
        self.assertEqual((status, out.strip()), (0, 'submitted=2 refused=1 ready=0'))

    def test_submitting_needs_the_manage_permission_not_just_the_view_permission(self):
        self.c.stack.service.provision_user('CG-4005', 'View only', PASSWORDS[4], 4, must_change_password=False, revokes=('routing.manage',))
        _, uri = self.c.stack.service.provision_user('CG-4006', 'View only', PASSWORDS[4], 4, must_change_password=False, revokes=('routing.manage',))
        self.c.secrets['CG-4006'] = parse_qs(urlparse(uri).query)['secret'][0]
        path, _ = claims_file(self.c, 1)
        status, out = self.c.run('submit', '--claims', str(path), badge='CG-4006', password=PASSWORDS[4])
        self.assertEqual((status, out.strip()), (2, 'error: this operator may not do that'))
        status, _ = self.c.run('reconcile', badge='CG-4006', password=PASSWORDS[4])
        self.assertEqual(status, 0)

    def test_replay_of_a_stored_claim_exits_zero_and_of_a_damaged_one_exits_one(self):
        path, rows = claims_file(self.c, 3)
        self.c.run('submit', '--claims', str(path))
        cid = rows[0]['claim_id']
        status, out = self.c.run('replay', cid)
        self.assertEqual(status, 0)
        report = json.loads(out)
        self.assertEqual((report['same'], report['claim_intact'], report['results_intact']), (True, True, True))
        self.c.queue._docs[(cid, 1)]['claim']['notes'] = 'edited after intake'
        status, out = self.c.run('replay', cid)
        self.assertEqual(status, 1)
        self.assertFalse(json.loads(out)['claim_intact'])
        self.assertEqual(self.c.run('replay', 'NOPE')[0], 2)

    # ---- rerun
    def test_rerun_refuses_the_running_pack_lists_changes_on_a_dry_run_and_applies_only_with_the_flag(self):
        from yara_engine import pack_hash
        path, rows = claims_file(self.c, 3)
        self.c.engine = FakeEngine()
        old = Intake(self.c.queue, self.c.engine, self.c.stack.securitylog, lambda: self.c.clock_now, 'old-pack', 'e')
        for r in rows:
            old.submit(r)
        self.c.engine = FakeEngine({rows[0]['claim_id']: {'R001': ('FAIL', 'high')}})
        status, out = self.c.run('rerun', '--rule-pack', pack_hash())
        self.assertEqual(status, 2)
        status, out = self.c.run('rerun', '--rule-pack', 'old-pack')
        report = json.loads(out)
        self.assertEqual((status, report['dry_run'], report['changed'], report['created']), (0, True, [rows[0]['claim_id']], []))
        self.assertEqual(self.c.queue.get(rows[0]['claim_id'])['version'], 1)
        status, out = self.c.run('rerun', '--rule-pack', 'old-pack', '--apply')
        self.assertEqual(json.loads(out)['created'], [{'claim_id': rows[0]['claim_id'], 'version': 2}])
        self.assertEqual(self.c.queue.get(rows[0]['claim_id'])['version'], 2)

    # ---- dead letters
    def test_dead_letters_can_be_listed_and_replayed_once(self):
        path, rows = claims_file(self.c, 1)
        self.c.run('submit', '--claims', str(path))
        cid = rows[0]['claim_id']
        self.c.queue.transition(cid, 1, 'triaged', 'dead_lettered', 'system:worker', 5.0)
        self.c.queue.add_dead_letter({'dead_id': 'X1', 'claim_id': cid, 'version': 1, 'reason': 'RuntimeError', 'attempts': 4, 'at': 5.0})
        status, out = self.c.run('deadletters')
        self.assertEqual((status, out.strip()), (0, f'X1  {cid} v1  RuntimeError  attempts=4'))
        status, out = self.c.run('deadletter-replay', 'X1')
        self.assertEqual((status, self.c.queue.get(cid)['state']), (0, 'triaged'))
        self.assertEqual(self.c.run('deadletter-replay', 'X1')[0], 2)

    # ---- verify-deal
    def test_verify_deal_passes_for_a_genuine_deal_and_fails_when_the_stored_seed_was_changed(self):
        path, rows = claims_file(self.c, 8)
        self.c.run('submit', '--claims', str(path), '--process')
        set_cfg(self.c.queue, on_shift=('CG-3003', 'CG-3004'), slice_size=5, low_water=2)
        dispatcher = Dispatcher(self.c.queue, lambda: [Agent('CG-3003', L3), Agent('CG-3004', L3)], lambda: self.c.clock_now, rng_seed=77)
        deal = dispatcher.deal()
        self.assertTrue(deal.assigned)
        status, out = self.c.run('verify-deal', deal.deal_id)
        self.assertEqual(status, 0, out)
        for d in self.c.queue._deals:
            if d['deal_id'] == deal.deal_id:
                d['seed'] += 1
        status, out = self.c.run('verify-deal', deal.deal_id)
        self.assertEqual(status, 1)
        self.assertIn('DOES NOT match', out)
        self.assertEqual(self.c.run('verify-deal', 'nope')[0], 2)


if __name__ == '__main__':
    unittest.main()
