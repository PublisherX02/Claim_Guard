"""The desktop app's address rules and demo bridge (the window itself needs a screen and is checked by hand)."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'desktop'))
sys.path.insert(0, str(ROOT / 'src'))
import claimguard_desktop as d
import pyotp


class ServerAddress(unittest.TestCase):
    def test_good_addresses_are_normalised(self):
        self.assertEqual(d.clean_server_url('claims.example.org'), 'https://claims.example.org')
        self.assertEqual(d.clean_server_url(' https://claims.example.org:8443/ '), 'https://claims.example.org:8443')
        self.assertEqual(d.clean_server_url('https://claims.example.org/ui'), 'https://claims.example.org')
        self.assertEqual(d.clean_server_url('http://127.0.0.1:8443'), 'http://127.0.0.1:8443')
        self.assertEqual(d.clean_server_url('http://localhost:9'), 'http://localhost:9')

    def test_plain_http_to_another_machine_is_refused(self):
        for bad in ('http://claims.example.org', 'http://10.0.0.5:8443', 'http://127.0.0.1.evil.example'):
            with self.assertRaises(ValueError, msg=bad):
                d.clean_server_url(bad)

    def test_other_shapes_are_refused(self):
        for bad in ('', '   ', 'ftp://x.org', 'file:///c:/x', 'javascript:alert(1)', 'https://user:pw@x.org', 'https://x.org/other/path', 'https://'):
            with self.assertRaises(ValueError, msg=bad):
                d.clean_server_url(bad)


class DemoBridge(unittest.TestCase):
    def test_hands_out_a_login_for_the_level_with_a_live_code(self):
        secret = pyotp.random_base32()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'accounts.json'
            path.write_text(json.dumps({'CG-2002': {'level': 2, 'name': 'n', 'password': 'pw', 'secret': secret}}), encoding='utf-8')
            got = d.DemoBridge(path).demo_login(2)
            self.assertEqual((got['badge'], got['password']), ('CG-2002', 'pw'))
            self.assertTrue(pyotp.TOTP(secret).verify(got['code']))
            self.assertIsNone(d.DemoBridge(path).demo_login(4))


if __name__ == '__main__':
    unittest.main()
