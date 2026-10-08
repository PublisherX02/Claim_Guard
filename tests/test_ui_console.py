"""The reviewer console's static files: served only from a fixed table with their own strict Content-Security-Policy, every other route
keeps default-src 'none', and no script uses a markup sink (server text is untrusted and may only become text)."""
import re
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import ui
from access_api_world import ApiWorld
from fastapi.testclient import TestClient

UI_DIR = ROOT / 'src' / 'access' / 'ui'
OWN_SCRIPTS = ('lib.js', 'views-work.js', 'views-admin.js', 'app.js')
SINKS = re.compile(r'innerHTML|outerHTML|insertAdjacentHTML|document\.write|\beval\s*\(|new Function|setAttribute\(\s*[\'"]style|srcdoc|javascript:', re.I)


class ConsoleServing(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.w = ApiWorld()
        cls.w.app = __import__('access.api', fromlist=['x']).create_app(cls.w.service, cls.w.claims, cls.w.review_log, cls.w.log,
                                                                        cls.w.settings, clock=cls.w.clock, ui=True)
        cls.c = TestClient(cls.w.app)

    @classmethod
    def tearDownClass(cls):
        cls.c.close()
        cls.w.close()

    def test_the_page_and_its_files_are_served_with_the_console_policy(self):
        for name, kind in (('', 'text/html'), ('app.js', 'text/javascript'), ('style.css', 'text/css'), ('vendor/qrcode.js', 'text/javascript')):
            r = self.c.get('/ui/' + name)
            self.assertEqual(r.status_code, 200, name)
            self.assertTrue(r.headers['content-type'].startswith(kind), name)
            self.assertEqual(r.headers['content-security-policy'], ui.UI_CSP)
            self.assertEqual(r.headers['x-frame-options'], 'DENY')
            self.assertEqual(r.headers['cache-control'], 'no-store')

    def test_the_policy_allows_no_inline_script_or_style_and_no_other_origin(self):
        csp = ui.UI_CSP
        self.assertNotIn('unsafe-inline', csp)
        self.assertNotIn('unsafe-eval', csp)
        self.assertNotIn('http', csp)
        self.assertIn("script-src 'self'", csp)
        self.assertIn("frame-ancestors 'none'", csp)
        html = (UI_DIR / 'index.html').read_text(encoding='utf-8')
        self.assertIsNone(re.search(r'<script(?![^>]*\bsrc=)', html), 'inline script')
        self.assertNotIn(' style=', html)
        self.assertNotIn('onclick=', html)

    def test_the_root_goes_to_the_console(self):
        r = self.c.get('/', follow_redirects=False)
        self.assertEqual((r.status_code, r.headers['location']), (307, '/ui/'))

    def test_only_listed_files_exist(self):
        for name in ('../api.py', '..%2fapi.py', '%2e%2e/api.py', 'vendor/README.txt', 'nope.js', 'vendor/', '..\\api.py'):
            r = self.c.get('/ui/' + name)
            self.assertEqual(r.status_code, 404, name)
        self.assertNotIn('def ', self.c.get('/ui/%2e%2e/api.py').text)

    def test_api_routes_keep_the_strict_policy(self):
        for r in (self.c.get('/healthz'), self.c.get('/api/v1/auth/me'), self.c.get('/api/v1/nothing')):
            self.assertEqual(r.headers['content-security-policy'], "default-src 'none'")

    def test_no_script_uses_a_markup_sink(self):
        for name in OWN_SCRIPTS:
            text = (UI_DIR / name).read_text(encoding='utf-8')
            self.assertIsNone(SINKS.search(text), name)

    def test_the_console_holds_no_secret_or_remote_address(self):
        for path in UI_DIR.glob('*'):
            if path.is_file():
                text = path.read_text(encoding='utf-8')
                self.assertNotRegex(text, r'https?://(?!www\.w3\.org)', path.name)
                self.assertNotRegex(text, r'(?i)(password|secret|token)\s*[:=]\s*[\'"][A-Za-z0-9]{8,}', path.name)

    def test_the_scripts_parse(self):
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is not installed')
        for name in OWN_SCRIPTS:
            done = subprocess.run([node, '--check', str(UI_DIR / name)], capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, name + ': ' + done.stderr)


if __name__ == '__main__':
    unittest.main()
