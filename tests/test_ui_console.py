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
OWN_SCRIPTS = ('lib.js', 'notify.js', 'views-work.js', 'views-fix.js', 'views-admin.js', 'views-ops.js', 'views-import.js', 'views-rules.js', 'views-ask.js', 'views-me.js', 'app.js')
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


class AdminScreens(unittest.TestCase):
    """What the administrator's screens must do (found in the browser test of 2026-10-18)."""

    def setUp(self):
        self.text = (UI_DIR / 'views-admin.js').read_text(encoding='utf-8')

    def test_one_submission_is_one_batch(self):
        # claims were once sent 20 at a time, so a 25-claim paste became two batches (and two audit samples)
        self.assertNotRegex(self.text, r'claims\.slice\(\s*i\s*,')
        self.assertRegex(self.text, r"CG\.api\('POST', '/queue/submit', \{ claims: claims, async: true \}\)")
        self.assertIn('var MAX_BATCH = 1000;', self.text)

    def test_routing_settings_carry_the_auto_clear_switch_and_the_audit_percent(self):
        # without the switch an administrator could not turn the batch audit on from the console
        for needle in ('auto_clear_green', 'audit_percent', 'Clear clean claims automatically', 'Percent of each batch that officers check'):
            self.assertIn(needle, self.text, needle)

    def test_the_held_tile_has_no_stray_label(self):
        self.assertNotIn("'held batches', d.held ? 'bad'", self.text)


class PersonalTemplates(unittest.TestCase):
    """The Customize page and the role templates (docs/62): looks and layout only, never access."""

    def test_an_imported_template_keeps_only_known_settings_with_allowed_values(self):
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is not installed')
        script = ("global.window = {}; global.document = { addEventListener: function () {} }; require(%r); var CG = window.CG;"
                  "var out = CG.cleanPrefs({accent: 'red;}</style>', theme: 'dark', home: 'javascript:alert(1)', isAdmin: true,"
                  " permissions: ['users.manage'], hiddenNav: ['#/inbox', '<img src=x>'], showAi: 'yes', density: 'compact'});"
                  "console.log(JSON.stringify(out));") % str(UI_DIR / 'lib.js')
        done = subprocess.run([node, '-e', script], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        import json
        self.assertEqual(json.loads(done.stdout), {'theme': 'dark', 'density': 'compact', 'hiddenNav': ['#/inbox']})

    def _run_lib(self, body):
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is not installed')
        done = subprocess.run([node, '-e', "global.window = {}; global.document = { addEventListener: function () {} }; require(%r); var CG = window.CG; %s" % (str(UI_DIR / 'lib.js'), body)], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        import json
        return json.loads(done.stdout)

    def test_backgrounds_sounds_and_pictures_are_allow_listed(self):
        out = self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({bg: 'photo', bgDim: 'strong', glass: true, notifySound: 'chime', notifyVolume: 'loud',"
                            " notify: 'yes', notifyDesktop: true, bgUrl: 'javascript:x'})))")
        self.assertEqual(out, {'bg': 'photo', 'bgDim': 'strong', 'glass': True, 'notifySound': 'chime', 'notifyDesktop': True})
        ok = self._run_lib("console.log(JSON.stringify([CG.photoOk('data:image/jpeg;base64,/9j/4AAQSk=='), CG.photoOk('data:image/png;base64,AAA\\\")) red'),"
                           " CG.photoOk('data:image/svg+xml;base64,PHN2Zz4='), CG.photoOk('https://example.com/x.jpg'), CG.photoOk(null)]))")
        self.assertEqual(ok, [True, False, False, False, False])
        self.assertEqual(self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({focus: 'spotlight'})))"), {'focus': 'spotlight'})
        self.assertEqual(self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({focus: 'blink'})))"), {})

    def test_accessibility_settings_are_allow_listed_and_every_preset_is_valid(self):
        out = self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({vision: 'redgreen', shapes: true, spacing: 'wide', font: 'readable', bigFocus: true, ruler: true,"
                            " vision2: 'x', spacing_: 1})))")
        self.assertEqual(out, {'vision': 'redgreen', 'shapes': True, 'spacing': 'wide', 'font': 'readable', 'bigFocus': True, 'ruler': True})
        self.assertEqual(self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({vision: 'url(x)', spacing: 'huge', ruler: 'yes'})))"), {})
        bad = self._run_lib("console.log(JSON.stringify(CG.A11Y_PRESETS.filter(function (p) { var c = CG.cleanPrefs(p[2]);"
                            " return Object.keys(p[2]).length !== Object.keys(c).length; }).map(function (p) { return p[0]; })))")
        self.assertEqual(bad, [])

    def test_deep_settings_are_filtered_too(self):
        out = self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({accent: 'custom', accentHex: 'red;background:url(x)', corners: 'round', font: 'comic',"
                            " homeWidgets: ['tiles', 'tiles', 'evil', 'clearance'], hiddenCols: ['priority', 'claim'], rail: true, sidebar: 'neon'})))")
        self.assertEqual(out, {'accent': 'custom', 'corners': 'round', 'rail': True, 'homeWidgets': ['tiles', 'clearance'], 'hiddenCols': ['priority']})
        self.assertEqual(self._run_lib("console.log(JSON.stringify(CG.cleanPrefs({accentHex: '#1D5FD1'})))"), {'accentHex': '#1d5fd1'})

    def test_a_custom_accent_always_gets_readable_text_and_profile_names_hold_no_markup(self):
        out = self._run_lib("console.log(JSON.stringify([CG.inkFor('#ffff00'), CG.inkFor('#1d1d6b'), CG.cleanName('<b>Night</b> shift\\u0007')]))")
        self.assertEqual(out, ['#0b1218', '#ffffff', 'bNight/b shift'])

    def test_every_role_has_a_template(self):
        text = (UI_DIR / 'lib.js').read_text(encoding='utf-8')
        for level in ('1:', '2:', '3:', '4:'):
            self.assertRegex(text, r'CG\.PRESETS = \{[\s\S]*\b' + level)

    def test_only_home_customize_and_brand_are_open_to_every_signed_in_person(self):
        text = (UI_DIR / 'app.js').read_text(encoding='utf-8')
        open_routes = re.findall(r"\[/\^#\\/(\w+)\$/, null,", text)
        self.assertEqual(sorted(open_routes), ['brand', 'customize', 'home'])


class ReviewPageFixes(unittest.TestCase):
    """The second walkthrough: the decision stays near the top, a viewer is told the truth, and the cause of a flag is shown with its evidence."""

    def _node(self, body):
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is not installed')
        script = "global.window = { CG: { h: function () { return {}; }, chip: function () {}, fmtValue: String } }; require(%r); %s" % (str(UI_DIR / 'views-fix.js'), body)
        done = subprocess.run([node, '-e', script], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout.strip()

    def test_the_copilot_sentence_is_kept_for_the_finding_card_without_its_prefix(self):
        out = self._node("""var m = window.CG.copilotWhy({ summary: { items: [{ rule_id: 'R013', sentence: 'R013 failed (medium severity): unit price 2210 is above the limit of 2200 for SVC-IMAGE.' }] } });
            console.log(JSON.stringify(m)); console.log(JSON.stringify(window.CG.copilotWhy({ summary: { cleared: true, items: [{ rule_id: 'R001', sentence: 'x' }] } })));""")
        first, second = out.splitlines()
        self.assertEqual(first, '{"R013":"unit price 2210 is above the limit of 2200 for SVC-IMAGE."}')
        self.assertEqual(second, '{}')

    def test_the_long_copilot_guidance_sits_behind_a_disclosure(self):
        text = (UI_DIR / 'views-fix.js').read_text(encoding='utf-8')
        self.assertIn("class: 'copilot-more'", text)
        self.assertIn('What to check, step by step', text)

    def test_a_viewer_is_not_told_that_an_unreviewed_finding_is_settled(self):
        text = (UI_DIR / 'views-work.js').read_text(encoding='utf-8')
        self.assertIn("review_state !== 'resolved'", text)
        self.assertIn('still open (read-only)', text)

    def test_evidence_about_the_named_line_is_highlighted_and_listed_first(self):
        text = (UI_DIR / 'views-work.js').read_text(encoding='utf-8')
        self.assertIn("'/lines/' + i + '/'", text)
        self.assertIn('f.affected_line_ids', text)

    def test_the_phone_layout_shows_a_bar_and_one_bell(self):
        css = (UI_DIR / 'style.css').read_text(encoding='utf-8')
        self.assertIn('.tablewrap td .bar', css)
        self.assertIn('.idbar .bell { display: none; }', css)


class DemoStaff(unittest.TestCase):
    """The demo's staff list for the sign-in page: names and levels, never a password or seed, and only for this PC."""

    def test_the_staff_list_holds_no_secret_and_answers_only_locally(self):
        import importlib.util
        from fastapi import FastAPI
        spec = importlib.util.spec_from_file_location('serve_access', ROOT / 'scripts' / 'serve_access.py')
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        app = FastAPI()
        mod._offer_demo_logins(app, {'CG-1001': {'level': 1, 'name': 'Nadia Haddad', 'title': 'Claims analyst (read-only)', 'password': 'pw-secret', 'secret': 'JBSWY3DPEHPK3PXP'}})
        local = TestClient(app, client=('127.0.0.1', 5000))
        r = local.get('/demo/people')
        self.assertEqual(r.json(), [{'badge': 'CG-1001', 'name': 'Nadia Haddad', 'level': 1, 'title': 'Claims analyst (read-only)'}])
        self.assertNotIn('pw-secret', r.text); self.assertNotIn('JBSWY3DP', r.text)
        self.assertEqual(local.get('/demo/login/1?badge=CG-9999').status_code, 404)
        self.assertEqual(local.get('/demo/login/1?badge=CG-1001').json()['badge'], 'CG-1001')
        remote = TestClient(app, client=('203.0.113.9', 5000))
        self.assertEqual(remote.get('/demo/people').status_code, 404)

    def test_the_arrival_feed_logs_new_claims_and_the_simulator_needs_the_demo_header(self):
        import importlib.util
        from fastapi import FastAPI
        from workqueue.demo import ArrivalFeed
        spec = importlib.util.spec_from_file_location('serve_access', ROOT / 'scripts' / 'serve_access.py')
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)

        class Store:
            def __init__(self): self.docs = {}
            def get(self, claim_id, version=None): return self.docs.get(claim_id)

        class Intake:
            def __init__(self, store): self.store = store
            def submit(self, claim):
                if claim.get('bad'):
                    raise ValueError('claim refused')
                old = self.store.docs.get(claim['claim_id'])
                if old is None or old['hash'] != claim.get('hash'):
                    self.store.docs[claim['claim_id']] = {'version': (old['version'] + 1) if old else 1, 'hash': claim.get('hash')}
                return {'lane': 'A'}

        class Queue:
            def __init__(self): self.store = Store(); self.intake = Intake(self.store)

        queue = Queue()
        feed = ArrivalFeed(queue, [{'claim_id': 'CG-AAA'}, {'claim_id': 'CG-BAD', 'bad': True}, {'claim_id': 'CG-BBB'}])
        app = FastAPI()
        mod._offer_demo_logins(app, {}, feed)
        local = TestClient(app, client=('127.0.0.1', 5000))
        self.assertEqual(local.get('/demo/feed').json(), {'seq': 0, 'recent': []})
        self.assertEqual(local.post('/demo/arrivals?n=2').status_code, 400)                      # no demo header: refused
        self.assertEqual(local.post('/demo/arrivals?n=9', headers={'X-CG-Demo': '1'}).status_code, 400)
        r = local.post('/demo/arrivals?n=2', headers={'X-CG-Demo': '1'})
        self.assertEqual(r.json(), {'accepted': 2, 'left': 0})                                    # the refused claim is skipped
        queue.intake.submit({'claim_id': 'CG-AAA'})                                               # the same claim again: not news
        queue.intake.submit({'claim_id': 'CG-CCC'})                                               # another way in (Submit page): logged
        snap = local.get('/demo/feed').json()
        self.assertEqual(snap['seq'], 3)
        self.assertEqual([x['claim_id'] for x in snap['recent']], ['CG-AAA', 'CG-BBB', 'CG-CCC'])
        remote = TestClient(app, client=('203.0.113.9', 5000))
        self.assertEqual(remote.get('/demo/feed').status_code, 404)
        self.assertEqual(remote.post('/demo/arrivals', headers={'X-CG-Demo': '1'}).status_code, 404)
