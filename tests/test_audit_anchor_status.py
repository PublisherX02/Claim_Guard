"""verify_audit.py must say whether the anchor was signed, not just print 'Chain OK'.

Without AUDIT_ANCHOR_KEY the anchor is unsigned, so whoever can write the log can rewrite log and anchor together and
still verify (docs/20, finding F4). The verifier cannot fix that, but it must not let a reader assume otherwise.
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
import verify_audit
from audit_log import ANCHOR_KEY_ENV, AuditLog, anchor_status

EVENTS = [{'event_type': 'run_started', 'run_id': 'run-1', 'claim_id': 'CG-1', 'input_hash': 'h'},
          {'event_type': 'rule_check', 'run_id': 'run-1', 'claim_id': 'CG-1', 'rule_id': 'R001', 'status': 'PASS'},
          {'event_type': 'run_finished', 'run_id': 'run-1', 'claim_id': 'CG-1'}]


def make_log(tmp, key):
    env = {ANCHOR_KEY_ENV: key} if key else {}
    with mock.patch.dict(os.environ, env, clear=False):
        if not key:
            os.environ.pop(ANCHOR_KEY_ENV, None)
        path = Path(tmp) / 'audit.jsonl'
        AuditLog(path)._write(EVENTS)
    return path


def run_cli(args, key=None):
    out = io.StringIO()
    env = {ANCHOR_KEY_ENV: key} if key else {}
    with mock.patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(out):
        if not key:
            os.environ.pop(ANCHOR_KEY_ENV, None)
        try:
            verify_audit.main(args)
            code = 0
        except SystemExit as e:
            if isinstance(e.code, str):               # SystemExit('message') prints the message to stderr on exit
                out.write(e.code)
            code = e.code if isinstance(e.code, int) else 1
    return code, out.getvalue()


class AnchorStatusTests(unittest.TestCase):
    def test_an_unkeyed_log_reports_an_unsigned_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key=None)
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop(ANCHOR_KEY_ENV, None)
                self.assertEqual(anchor_status(path), {'key_configured': False, 'anchor_signed': False})

    def test_a_keyed_log_reports_a_signed_anchor_and_a_configured_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key='k1')
            with mock.patch.dict(os.environ, {ANCHOR_KEY_ENV: 'k1'}):
                self.assertEqual(anchor_status(path), {'key_configured': True, 'anchor_signed': True})

    def test_a_signed_anchor_without_a_key_here_is_reported_as_unchecked(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key='k1')
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop(ANCHOR_KEY_ENV, None)
                self.assertEqual(anchor_status(path), {'key_configured': False, 'anchor_signed': True})

    def test_a_missing_or_garbled_anchor_never_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(anchor_status(Path(tmp) / 'nothing.jsonl')['anchor_signed'], False)
            path = make_log(tmp, key=None)
            path.with_name(path.name + '.head.json').write_text('not json', encoding='utf-8')
            self.assertEqual(anchor_status(path)['anchor_signed'], False)


class VerifyAuditCliTests(unittest.TestCase):
    def test_unsigned_anchor_is_warned_about_but_still_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key=None)
            code, out = run_cli(['--log', str(path)])
            self.assertEqual(code, 0)
            self.assertIn('Chain OK', out)
            self.assertIn('WARNING: the anchor is NOT signed', out)

    def test_require_key_fails_an_unsigned_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key=None)
            code, out = run_cli(['--log', str(path), '--require-key'])
            self.assertNotEqual(code, 0)
            self.assertIn('--require-key', out)

    def test_a_signed_anchor_with_the_key_passes_require_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key='k1')
            code, out = run_cli(['--log', str(path), '--require-key'], key='k1')
            self.assertEqual(code, 0)
            self.assertIn('Anchor: HMAC-signed and verified', out)
            self.assertNotIn('WARNING', out)

    def test_a_signed_anchor_without_the_key_is_not_claimed_as_verified(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp, key='k1')
            code, out = run_cli(['--log', str(path)])
            self.assertEqual(code, 0)
            self.assertIn('signature was NOT checked', out)
            code, _ = run_cli(['--log', str(path), '--require-key'])
            self.assertNotEqual(code, 0)


if __name__ == '__main__':
    unittest.main()
