"""The Rust deployment files (deploy/rust) keep the same safety promises as the Python ones: only the proxy publishes a port, nothing runs as
root, no secret has a default, the container runs the Rust binary, and the build leaves out secrets and the build directory."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEPLOY = ROOT / 'deploy' / 'rust'


def services(text):
    body = text.split('\nservices:\n', 1)[1].split('\nnetworks:\n', 1)[0].split('\nvolumes:\n', 1)[0]
    parts = re.split(r'\n(?=  [a-z]+:[^\n]*\n)', '\n' + body)
    return {re.match(r'\s*([a-z]+):', p).group(1): p for p in parts if re.match(r'\s*[a-z]+:[^\n]*\n', p)}


class RustCompose(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (DEPLOY / 'compose.yml').read_text(encoding='utf-8')
        cls.svc = services(cls.text)

    def test_the_same_services_as_the_python_stack(self):
        self.assertEqual(set(self.svc), {'proxy', 'api', 'worker', 'mongo', 'redis', 'ollama', 'reader'})

    def test_only_the_proxy_publishes_ports(self):
        for name, block in self.svc.items():
            self.assertEqual('ports:' in block, name == 'proxy', name)

    def test_every_required_secret_is_demanded_not_defaulted(self):
        for name in ('JWT_SECRET', 'FERNET_KEY', 'AUDIT_ANCHOR_KEY', 'PII_KEY', 'MONGO_ROOT_PASSWORD'):
            self.assertRegex(self.text, r'\$\{' + name + r':\?', name)
            self.assertNotRegex(self.text, r'\$\{' + name + r':-', name)

    def test_dual_control_is_on_by_default_and_the_old_data_key_can_be_kept(self):
        self.assertRegex(self.text, r'DUAL_CONTROL: \$\{DUAL_CONTROL:-true\}')
        self.assertRegex(self.text, r'DATA_KEY_PREVIOUS: \$\{DATA_KEY_PREVIOUS:-\}')
        example = (ROOT / 'deploy' / 'env.production.example').read_text(encoding='utf-8')
        self.assertIn('DUAL_CONTROL=true', example)
        self.assertIn('DATA_KEY_PREVIOUS=', example)

    def test_app_containers_are_locked_down(self):
        head = self.text.split('\nservices:\n', 1)[0]
        for needle in ('no-new-privileges:true', 'cap_drop: [ALL]', 'read_only: true'):
            self.assertIn(needle, head)

    def test_the_commands_run_the_rust_binary_and_only_the_worker_runs_the_loops(self):
        self.assertIn('"cg", "serve"', self.svc['api'])
        self.assertIn('"--behind-proxy"', self.svc['api'])
        self.assertIn('"cg", "worker"', self.svc['worker'])
        self.assertNotIn('python', self.text.lower().replace('# ', ' '))
        self.assertNotIn('replicas', self.text)
        self.assertNotIn('"--demo"', self.text)
        self.assertNotIn('"--dev"', self.text)

    def test_the_database_and_cache_sit_on_a_network_with_no_route_out_and_the_proxy_cannot_see_them(self):
        nets = self.text.split('\nnetworks:\n', 1)[1].split('\nvolumes:\n', 1)[0]
        self.assertRegex(nets, r'data:\s*\n\s+internal: true')
        self.assertIn('networks: [edge]', self.svc['proxy'])
        self.assertIn('networks: [edge, data, reader]', self.svc['api'])
        self.assertRegex(nets, r'reader:\s*\n\s+internal: true')      # the document reader has no route out either
        self.assertIn('networks: [reader]', self.svc['reader'])
        self.assertNotRegex(self.svc['reader'], r'networks:[^\n]*(edge|egress|data)')
        self.assertIn('networks: [data]', self.svc['mongo'])
        self.assertIn('networks: [data]', self.svc['redis'])
        self.assertNotRegex(self.svc['proxy'], r'networks:[^\n]*data')

    def test_redis_demands_a_password_and_the_app_containers_have_limits(self):
        self.assertIn('--requirepass', self.svc['redis'])
        self.assertRegex(self.text, r'REDIS_PASSWORD:\s*\$\{REDIS_PASSWORD:\?')
        head = self.text.split('\nservices:\n', 1)[0]
        for needle in ('mem_limit:', 'pids_limit:', 'cpus:'):
            self.assertIn(needle, head)

    def test_every_container_has_a_memory_limit_and_the_database_and_broker_are_capped_inside_it(self):
        head = self.text.split('\nservices:\n', 1)[0]
        self.assertIn('mem_limit:', head)  # api and worker share it
        for name in ('proxy', 'mongo', 'redis', 'ollama'):
            self.assertIn('mem_limit:', self.svc[name], name)
        # MongoDB would otherwise take half of the machine for its cache
        self.assertRegex(self.svc['mongo'], r'--wiredTigerCacheSizeGB\W+\$\{MONGO_CACHE_GB:-1\}')
        # and Redis refuses to publish rather than silently evicting queued work
        self.assertIn('--maxmemory 200mb', self.svc['redis'])
        self.assertIn('--maxmemory-policy noeviction', self.svc['redis'])

    def test_only_the_worker_mounts_the_audit_mirror(self):
        self.assertIn('audit_mirror:/mirror', self.svc['worker'])
        for name in ('api', 'proxy', 'mongo', 'redis'):
            self.assertNotIn('audit_mirror', self.svc[name], name)

    def test_paths_resolve_from_the_compose_file_location(self):
        self.assertIn('context: ../..', self.text)
        self.assertIn('dockerfile: deploy/rust/Dockerfile', self.text)
        self.assertTrue((DEPLOY.parent / 'Caddyfile.internal').exists())
        self.assertIn('../${CADDYFILE:-Caddyfile.internal}', self.text)


class RustImage(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (DEPLOY / 'Dockerfile').read_text(encoding='utf-8')

    def test_runs_as_a_non_root_user_behind_the_proxy(self):
        self.assertRegex(self.text, r'(?m)^USER 65532(:65532)?$')
        self.assertIn('FROM gcr.io/distroless/cc-debian12:nonroot', self.text)
        self.assertNotIn('apt-get', self.text.split('FROM gcr.io/distroless')[1])
        self.assertIn('--behind-proxy', self.text)
        self.assertNotIn('--dev', self.text)
        self.assertNotIn('--demo', self.text)

    def test_the_binary_is_built_from_the_lockfile_and_nothing_secret_is_copied(self):
        self.assertIn('--locked', self.text)
        for bad in ('.env', 'instance', '.git'):
            self.assertNotRegex(self.text, r'(?m)^COPY\s+' + re.escape(bad))

    def test_the_build_context_leaves_out_the_rust_build_directory(self):
        ignore = (ROOT / '.dockerignore').read_text(encoding='utf-8').split()
        for item in ('rust/target', '.env', '.git', '.venv'):
            self.assertIn(item, ignore)

    def test_the_image_has_a_health_check_that_needs_no_extra_tools(self):
        self.assertIn('"cg", "health"', self.text)


class RustSupplyChain(unittest.TestCase):
    PIN = r'@sha256:[0-9a-f]{64}'

    def test_every_third_party_image_is_pinned_by_digest(self):
        compose = (DEPLOY / 'compose.yml').read_text(encoding='utf-8')
        for line in re.findall(r'(?m)^\s+image: (\S+)', compose):
            if line.startswith(('claimguard-rs', 'claimguard-reader')):
                continue                                   # our own images, built from the Dockerfiles
            self.assertRegex(line, self.PIN, line)
        for df in (DEPLOY / 'Dockerfile', DEPLOY.parent / 'ocr' / 'Dockerfile'):
            for line in re.findall(r'(?m)^FROM (\S+)', df.read_text(encoding='utf-8')):
                self.assertRegex(line, self.PIN, line)
        for name in ('backup.sh', 'restore.sh'):
            for image in re.findall(r'docker run [^\n]*? (alpine\S*) sh -c', (DEPLOY / name).read_text(encoding='utf-8')):
                self.assertRegex(image, self.PIN, (name, image))

    def test_backups_are_encrypted_checksummed_and_never_overwrite(self):
        backup = (DEPLOY / 'backup.sh').read_text(encoding='utf-8')
        for needle in ('set -euo pipefail', 'openssl enc -aes-256-cbc -pbkdf2 -iter 600000', 'umask 077', 'sha256sum', 'not overwriting', 'BACKUP_KEY_FILE'):
            self.assertIn(needle, backup, needle)
        self.assertNotIn('--password', backup.replace('"$MONGO_INITDB_ROOT_PASSWORD"', ''))
        restore = (DEPLOY / 'restore.sh').read_text(encoding='utf-8')
        for needle in ('set -euo pipefail', 'sha256sum -c', 'wrong key', '--drop', '--nsInclude', 'did not restore'):
            self.assertIn(needle, restore, needle)


class RustProxy(unittest.TestCase):
    def test_both_proxy_configs_cut_off_slow_and_oversized_requests(self):
        for name in ('Caddyfile.internal', 'Caddyfile.public'):
            text = (DEPLOY.parent / name).read_text(encoding='utf-8')
            for needle in ('read_header 10s', 'read_body 30s', 'max_header_size 16KB', 'max_size 100KB', 'Strict-Transport-Security'):
                self.assertIn(needle, text, (name, needle))


class RustInstall(unittest.TestCase):
    def test_install_never_overwrites_an_existing_env_and_runs_the_rust_tools(self):
        text = (DEPLOY / 'install.sh').read_text(encoding='utf-8')
        self.assertIn('set -euo pipefail', text)
        self.assertRegex(text, r'if \[ -e \.env \]')
        self.assertIn('umask 077', text)
        self.assertIn('deploy/rust/compose.yml', text)
        self.assertIn('cg admin create-admin', text)
        self.assertNotIn('python', text.lower().replace('# ', ' '))
        self.assertNotIn('deploy/compose.yml', text.replace('deploy/rust/compose.yml', ''))


if __name__ == '__main__':
    unittest.main()
