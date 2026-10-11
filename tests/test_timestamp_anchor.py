"""RFC 3161 timestamp on the audit anchor (src/timestamp_anchor.py). A local Time-Stamp Authority signs real tokens; no network.

What must hold: a good token verifies; a token for another head, a forged signature, a wrong nonce, an untrusted issuer,
an expired or non-timestamping certificate and a refused request are all rejected; a TSA outage never blocks writing the
log; a log that was rewritten after stamping is reported invalid; logs with no token still verify as before.
"""
import json
import os
import sys
import tempfile
import unittest
import urllib.error
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'tests'))
import timestamp_anchor as ta
from audit_log import ANCHOR_KEY_ENV, AuditLog
from local_tsa import LocalTSA, make_ca, make_tsa_cert

EVENTS = [{'event_type': 'run_started', 'run_id': 'run-1', 'claim_id': 'CG-1', 'input_hash': 'h'},
          {'event_type': 'rule_check', 'run_id': 'run-1', 'claim_id': 'CG-1', 'rule_id': 'R001', 'status': 'PASS'},
          {'event_type': 'run_finished', 'run_id': 'run-1', 'claim_id': 'CG-1'}]
HEAD, COUNT = 'ab' * 32, 7


def req_for(head=HEAD, count=COUNT, nonce=12345):
    return ta.build_request(ta.imprint(head, count), nonce)


class TokenVerification(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tsa = LocalTSA()

    def good(self, **kw):
        return self.tsa.respond(req_for(), **kw)

    def test_valid_token_verifies_and_reports_time_and_signer(self):
        info = ta.verify_token(self.good(), HEAD, COUNT, self.tsa.trusted_pem, expected_nonce=12345)
        self.assertIn('Test TSA', info['tsa_subject'])
        self.assertTrue(info['gen_time'].startswith(str(datetime.now(timezone.utc).year)))

    def test_token_for_another_head_or_count_is_rejected(self):
        for head, count in (('cd' * 32, COUNT), (HEAD, COUNT + 1)):
            with self.assertRaisesRegex(ta.TimestampError, 'different head or count'):
                ta.verify_token(self.good(), head, count, self.tsa.trusted_pem)

    def test_tsa_that_stamps_a_different_digest_is_rejected(self):
        with self.assertRaisesRegex(ta.TimestampError, 'different head or count'):
            ta.verify_token(self.good(tamper_imprint=True), HEAD, COUNT, self.tsa.trusted_pem)

    def test_forged_signature_is_rejected(self):
        with self.assertRaisesRegex(ta.TimestampError, 'signature does not verify'):
            ta.verify_token(self.good(tamper_signature=True), HEAD, COUNT, self.tsa.trusted_pem)

    def test_flipped_byte_in_the_signed_content_is_rejected(self):
        der = bytearray(self.good())
        # Flip a byte inside the stamped time (the TSTInfo is signed through its digest).
        idx = bytes(der).index(b'Z', bytes(der).index(datetime.now(timezone.utc).strftime('%Y%m%d').encode()))
        der[idx - 1] ^= 1
        with self.assertRaises(ta.TimestampError):
            ta.verify_token(bytes(der), HEAD, COUNT, self.tsa.trusted_pem)

    def test_wrong_nonce_is_rejected(self):
        with self.assertRaisesRegex(ta.TimestampError, 'nonce'):
            ta.verify_token(self.good(), HEAD, COUNT, self.tsa.trusted_pem, expected_nonce=999)

    def test_untrusted_issuer_is_rejected(self):
        other_ca_key, other_ca = make_ca('Some Other CA')
        with self.assertRaisesRegex(ta.TimestampError, 'not issued by a trusted CA'):
            ta.verify_token(self.good(), HEAD, COUNT, LocalTSA(other_ca_key, other_ca).trusted_pem)

    def test_certificate_embedded_in_the_token_is_never_trusted_by_itself(self):
        # A self-made TSA whose own CA the verifier does not trust.
        rogue = LocalTSA()
        with self.assertRaisesRegex(ta.TimestampError, 'not issued by a trusted CA'):
            ta.verify_token(rogue.respond(req_for()), HEAD, COUNT, self.tsa.trusted_pem)

    def test_certificate_without_timestamping_usage_is_rejected(self):
        for kw in ({'add_eku': False}, {'eku_critical': False}):
            key, cert = make_tsa_cert(self.tsa.ca_key, self.tsa.ca_cert, **kw)
            tsa = LocalTSA(self.tsa.ca_key, self.tsa.ca_cert, key, cert)
            with self.assertRaisesRegex(ta.TimestampError, 'timeStamping|extended key usage'):
                ta.verify_token(tsa.respond(req_for()), HEAD, COUNT, tsa.trusted_pem)

    def test_certificate_not_valid_at_the_stamped_time_is_rejected(self):
        past = datetime.now(timezone.utc) - timedelta(days=20)
        tsa = LocalTSA(self.tsa.ca_key, self.tsa.ca_cert, *make_tsa_cert(self.tsa.ca_key, self.tsa.ca_cert), gen_time=past)
        with self.assertRaisesRegex(ta.TimestampError, 'not valid at the stamped time'):
            ta.verify_token(tsa.respond(req_for()), HEAD, COUNT, tsa.trusted_pem)

    def test_refused_request_is_rejected(self):
        with self.assertRaisesRegex(ta.TimestampError, 'refused'):
            ta.verify_token(self.tsa.respond(req_for(), status=2), HEAD, COUNT, self.tsa.trusted_pem)

    def test_garbage_and_trailing_bytes_are_rejected(self):
        for blob in (b'', b'not der', b'\x30\x00', self.good() + b'\x00'):
            with self.assertRaises(ta.TimestampError):
                ta.verify_token(blob, HEAD, COUNT, self.tsa.trusted_pem)

    def test_no_trusted_ca_supplied_is_rejected(self):
        with self.assertRaises(Exception):
            ta.verify_token(self.good(), HEAD, COUNT, b'')


class LogIntegration(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.tsa = LocalTSA()
        with mock.patch.dict(os.environ):
            os.environ.pop(ANCHOR_KEY_ENV, None)
            self.log_path = Path(self.tmp.name) / 'audit.jsonl'
            self.log = AuditLog(self.log_path)
            self.log._write(EVENTS)
        self.anchor = self.log.anchor_path

    def status(self, trusted=True):
        return ta.timestamp_status(self.log_path, self.anchor, self.tsa.trusted_pem if trusted else None)

    def stamp(self):
        return ta.stamp_log(self.log_path, self.anchor, 'http://tsa.invalid/', opener=self.tsa.opener())

    def test_log_without_a_token_is_reported_none_and_still_verifies(self):
        from audit_log import verify_with_anchor
        self.assertEqual(self.status()['state'], 'none')
        self.assertEqual(verify_with_anchor(self.log_path, strict=True)[1], len(EVENTS))

    def test_stamped_log_is_current_then_stale_after_new_events(self):
        self.stamp()
        self.assertEqual(self.status()['state'], 'current')
        self.log._write([{'event_type': 'run_finished', 'run_id': 'run-2', 'claim_id': 'CG-2'}])
        st = self.status()
        self.assertEqual((st['state'], st['unstamped_events']), ('stale', 1))
        self.stamp()
        self.assertEqual(self.status()['state'], 'current')

    def test_sidecar_without_trusted_ca_is_unverified_not_current(self):
        self.stamp()
        self.assertEqual(self.status(trusted=False)['state'], 'unverified')

    def test_log_rewritten_after_stamping_is_invalid(self):
        self.stamp()
        rows = [json.loads(line) for line in self.log_path.read_text(encoding='utf-8').splitlines()]
        rows[0]['event']['claim_id'] = 'CG-TAMPERED'
        # Rebuild a consistent chain and anchor (what an attacker holding no HMAC key could do) ...
        other = Path(self.tmp.name) / 'forged.jsonl'
        forged = AuditLog(other)
        forged._write([r['event'] for r in rows])
        forged_log = Path(self.tmp.name) / 'audit.jsonl'
        os.replace(other, forged_log)
        os.replace(forged.anchor_path, self.anchor)
        # ... the timestamp token still pins the original head.
        st = self.status()
        self.assertEqual(st['state'], 'invalid')
        self.assertIn('rewritten or replaced', st['reason'])

    def test_tampered_sidecar_is_invalid(self):
        self.stamp()
        sp = ta.sidecar_path(self.log_path)
        side = json.loads(sp.read_text())
        side['count'] = 1
        sp.write_text(json.dumps(side))
        self.assertEqual(self.status()['state'], 'invalid')

    def test_tsa_outage_raises_and_keeps_the_previous_token(self):
        self.stamp()
        before = ta.sidecar_path(self.log_path).read_text()
        self.tsa.fail_with = urllib.error.URLError('connection refused')
        with self.assertRaises(ta.TimestampError):
            self.stamp()
        self.assertEqual(ta.sidecar_path(self.log_path).read_text(), before)

    def test_appends_never_touch_the_network(self):
        with mock.patch('urllib.request.urlopen', side_effect=AssertionError('network used by AuditLog')):
            self.log._write([{'event_type': 'run_finished', 'run_id': 'run-3', 'claim_id': 'CG-3'}])

    def test_oversized_response_is_refused(self):
        class Big:
            def read(self, n=-1):
                return b'x' * (ta.MAX_RESPONSE_BYTES + 1)

            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False
        with self.assertRaisesRegex(ta.TimestampError, 'larger than'):
            ta.stamp_log(self.log_path, self.anchor, 'http://tsa.invalid/', opener=lambda req, timeout=None: Big())


class CommandLines(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.tsa = LocalTSA()
        with mock.patch.dict(os.environ):
            os.environ.pop(ANCHOR_KEY_ENV, None)
            self.log_path = Path(self.tmp.name) / 'audit.jsonl'
            self.log = AuditLog(self.log_path)
            self.log._write(EVENTS)
        self.ca = Path(self.tmp.name) / 'ca.pem'
        self.ca.write_bytes(self.tsa.trusted_pem)

    def verify(self, *extra):
        import contextlib
        import io
        import verify_audit
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            try:
                verify_audit.main(['--log', str(self.log_path), *extra])
                code = 0
            except SystemExit as exc:
                code = exc.code or 0
        return code, out.getvalue()

    def test_verify_audit_reports_none_and_passes_without_the_requirement(self):
        code, out = self.verify()
        self.assertEqual(code, 0)
        self.assertIn('Timestamp: none', out)

    def test_require_timestamp_fails_when_none_then_passes_when_current(self):
        code, _ = self.verify('--require-timestamp', '--tsa-ca', str(self.ca))
        self.assertNotEqual(code, 0)
        ta.stamp_log(self.log_path, self.log.anchor_path, 'http://tsa.invalid/', opener=self.tsa.opener())
        code, out = self.verify('--require-timestamp', '--tsa-ca', str(self.ca))
        self.assertEqual(code, 0, out)
        self.assertIn('Timestamp: verified, covers the whole log', out)

    def test_invalid_timestamp_fails_verify_audit_even_without_the_flag(self):
        ta.stamp_log(self.log_path, self.log.anchor_path, 'http://tsa.invalid/', opener=self.tsa.opener())
        other = LocalTSA()
        bad_ca = Path(self.tmp.name) / 'other.pem'
        bad_ca.write_bytes(other.trusted_pem)
        code, out = self.verify('--tsa-ca', str(bad_ca))
        self.assertNotEqual(code, 0)
        self.assertIn('INVALID', out)

    def test_stamp_command_exit_codes(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('ts_cli', ROOT / 'scripts' / 'timestamp_log.py')
        cli = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cli)
        with mock.patch.object(ta, 'request_token', side_effect=ta.TimestampError('down')):
            self.assertEqual(cli.main(['stamp', '--log', str(self.log_path), '--tsa-url', 'http://tsa.invalid/']), 2)
        self.assertFalse(ta.sidecar_path(self.log_path).exists())
        self.assertEqual(cli.main(['verify', '--log', str(self.log_path)]), 0)


if __name__ == '__main__':
    unittest.main()
