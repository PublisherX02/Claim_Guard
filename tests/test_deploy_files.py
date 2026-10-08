"""The deployment files keep their safety promises: only the proxy publishes a port, nothing runs as root, no secret has a default,
and the install script cannot overwrite an existing .env."""
import re
import unittest
from pathlib import Path

DEPLOY = Path(__file__).resolve().parents[1] / 'deploy'


def services(text):
    """{service name: its block of text} for the top-level `services:` section of the compose file."""
    body = text.split('\nservices:\n', 1)[1].split('\nvolumes:\n', 1)[0]
    parts = re.split(r'\n(?=  [a-z]+:[^\n]*\n)', '\n' + body)
    return {re.match(r'\s*([a-z]+):', p).group(1): p for p in parts if re.match(r'\s*[a-z]+:[^\n]*\n', p)}


class Compose(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (DEPLOY / 'compose.yml').read_text(encoding='utf-8')
        cls.svc = services(cls.text)

    def test_the_expected_services_exist(self):
        self.assertEqual(set(self.svc), {'proxy', 'api', 'worker', 'mongo', 'redis', 'ollama'})

    def test_only_the_proxy_publishes_ports(self):
        for name, block in self.svc.items():
            self.assertEqual('ports:' in block, name == 'proxy', name)
        self.assertIn('"443:443"', self.svc['proxy'])

    def test_every_required_secret_is_demanded_not_defaulted(self):
        for name in ('JWT_SECRET', 'FERNET_KEY', 'AUDIT_ANCHOR_KEY', 'PII_KEY', 'MONGO_ROOT_PASSWORD'):
            self.assertRegex(self.text, r'\$\{' + name + r':\?', name)
            self.assertNotRegex(self.text, r'\$\{' + name + r':-', name)

    def test_app_containers_are_locked_down(self):
        head = self.text.split('\nservices:\n', 1)[0]
        for needle in ('no-new-privileges:true', 'cap_drop: [ALL]', 'read_only: true'):
            self.assertIn(needle, head)

    def test_the_optional_model_server_is_off_unless_asked_for(self):
        self.assertIn('profiles: ["ai"]', self.svc['ollama'])

    def test_one_worker_runs_the_scheduler(self):
        self.assertIn('"--beat"', self.svc['worker'])
        self.assertNotIn('--beat', self.svc['api'])
        self.assertNotIn('replicas', self.text)


class Image(unittest.TestCase):
    def test_runs_as_a_non_root_user_and_serves_behind_the_proxy(self):
        text = (DEPLOY / 'Dockerfile').read_text(encoding='utf-8')
        self.assertRegex(text, r'(?m)^USER claimguard$')
        self.assertIn('--behind-proxy', text)
        self.assertNotIn('--dev', text)
        self.assertNotIn('--demo', text)

    def test_the_build_context_leaves_out_secrets_and_the_environment(self):
        ignore = (DEPLOY.parent / '.dockerignore').read_text(encoding='utf-8').split()
        for item in ('.env', '.git', '.venv', 'instance'):
            self.assertIn(item, ignore)


class Scripts(unittest.TestCase):
    def test_install_never_overwrites_an_existing_env_and_keeps_it_private(self):
        text = (DEPLOY / 'install.sh').read_text(encoding='utf-8')
        self.assertIn('set -euo pipefail', text)
        self.assertRegex(text, r'if \[ -e \.env \]')
        self.assertIn('umask 077', text)
        self.assertNotRegex(text, r'echo "\$\{?(JWT|FERNET|PII|AUDIT)')

    def test_the_env_template_ships_no_secret_values(self):
        for line in (DEPLOY / 'env.production.example').read_text(encoding='utf-8').splitlines():
            if re.match(r'(JWT_SECRET|AUDIT_ANCHOR_KEY|PII_KEY|FERNET_KEY|MONGO_ROOT_PASSWORD)=', line):
                self.assertTrue(line.endswith('='), line)

    def test_both_proxy_configs_terminate_tls_and_hide_the_server_header(self):
        for name in ('Caddyfile.internal', 'Caddyfile.public'):
            text = (DEPLOY / name).read_text(encoding='utf-8')
            self.assertIn('reverse_proxy api:8443', text)
            self.assertIn('-Server', text)


if __name__ == '__main__':
    unittest.main()
