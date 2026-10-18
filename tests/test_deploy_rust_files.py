"""The Rust deployment files (deploy/rust) keep the same safety promises as the Python ones: only the proxy publishes a port, nothing runs as
root, no secret has a default, the container runs the Rust binary, and the build leaves out secrets and the build directory."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEPLOY = ROOT / 'deploy' / 'rust'


def services(text):
    body = text.split('\nservices:\n', 1)[1].split('\nnetworks:\n', 1)[0].split('\nvolumes:\n', 1)[0]
    parts = re.split(r'\n(?=  [a-z][a-z-]*:[^\n]*\n)', '\n' + body)
    return {re.match(r'\s*([a-z][a-z-]*):', p).group(1): p for p in parts if re.match(r'\s*[a-z][a-z-]*:[^\n]*\n', p)}


class RustCompose(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (DEPLOY / 'compose.yml').read_text(encoding='utf-8')
        cls.svc = services(cls.text)

    def test_the_same_services_as_the_python_stack(self):
        self.assertEqual(set(self.svc), {'proxy', 'api', 'worker', 'mongo', 'redis', 'vllm', 'vllm-vision', 'vllm-fetch', 'reader', 'embed'})

    def test_the_embedding_service_is_optional_has_no_route_out_and_demands_its_token(self):
        e = self.svc['embed']
        self.assertIn('profiles: ["search"]', e)
        self.assertIn('networks: [search]', e)
        self.assertNotRegex(e, r'networks:[^\n]*(edge|egress|data|reader)')
        self.assertRegex(self.text, r'search:\s*\n\s+internal: true')
        self.assertIn('EMBED_TOKEN: ${EMBEDDER_TOKEN:?', e)
        for flag in ('read_only: true', 'cap_drop: [ALL]', 'no-new-privileges:true'):
            self.assertIn(flag, e)
        self.assertIn('EMBEDDER_URL: ${EMBEDDER_URL:-}', self.text)      # empty by default: the keyword search answers
        df = (DEPLOY.parent / 'embed' / 'Dockerfile').read_text(encoding='utf-8')
        self.assertRegex(df, r"revision='[0-9a-f]{40}'")                     # the weights are pinned to one revision
        self.assertIn('HF_HUB_OFFLINE=1', df)

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
        for name in ('proxy', 'api', 'worker', 'mongo', 'redis'):  # the model servers and the reader are Python programs; the Rust build is not
            self.assertNotIn('python', self.svc[name].lower().replace('# ', ' '), name)
        self.assertNotIn('replicas', self.text)
        self.assertNotIn('"--demo"', self.text)
        self.assertNotIn('"--dev"', self.text)

    def test_the_database_and_cache_sit_on_a_network_with_no_route_out_and_the_proxy_cannot_see_them(self):
        nets = self.text.split('\nnetworks:\n', 1)[1].split('\nvolumes:\n', 1)[0]
        self.assertRegex(nets, r'data:\s*\n\s+internal: true')
        self.assertIn('networks: [edge]', self.svc['proxy'])
        self.assertIn('networks: [edge, data, reader, search]', self.svc['api'])
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
        for name in ('proxy', 'mongo', 'redis', 'vllm', 'vllm-vision', 'vllm-fetch'):
            self.assertIn('mem_limit:', self.svc[name], name)
        # MongoDB would otherwise take half of the machine for its cache
        self.assertRegex(self.svc['mongo'], r'--wiredTigerCacheSizeGB\W+\$\{MONGO_CACHE_GB:-1\}')
        # and Redis refuses to publish rather than silently evicting queued work
        self.assertIn('--maxmemory 200mb', self.svc['redis'])
        self.assertIn('--maxmemory-policy noeviction', self.svc['redis'])

    def test_the_model_servers_are_vllm_off_by_default_pinned_keyed_and_offline(self):
        for name, profile in (('vllm', 'ai'), ('vllm-vision', 'vision'), ('vllm-fetch', 'ai-fetch')):
            self.assertIn(f'profiles: ["{profile}"]', self.svc[name], name)
        for name in ('vllm', 'vllm-vision', 'vllm-fetch'):
            self.assertRegex(self.svc[name], r'image: \$\{VLLM_IMAGE:-vllm/vllm-openai:v\d+\.\d+\.\d+[^}]*\}', name)
        self.assertNotIn(':latest', self.text.split('services:', 1)[1].replace('# ', ' ').split('reader:', 1)[0].split('vllm:', 1)[-1])
        for name in ('vllm', 'vllm-vision'):
            block = self.svc[name]
            self.assertIn('HF_HUB_OFFLINE: "1"', block, name)           # the server never asks the internet for weights
            self.assertRegex(block, r'VLLM_API_KEY: \$\{VLLM_API_KEY:\?', name)   # the key is demanded, never defaulted
            self.assertIn('--api-key', block, name)
            self.assertIn('--served-model-name', block, name)
            self.assertIn('--host 0.0.0.0', block, name)
        self.assertIn('--max-num-seqs "$$VLLM_MAX_SEQS"', self.svc['vllm'])
        self.assertRegex(self.svc['vllm'], r'VLLM_MAX_SEQS: \$\{CG_WORKER_CONCURRENCY:-4\}')   # the server's slots follow the worker's pool
        self.assertIn('VLLM_EXTRA_ARGS: ${VLLM_EXTRA_ARGS:---quantization fp8}', self.svc['vllm'])
        gpu = float(re.search(r'VLLM_GPU_UTIL:-([0-9.]+)', self.svc['vllm']).group(1)) + float(re.search(r'VLLM_VLM_GPU_UTIL:-([0-9.]+)', self.svc['vllm-vision']).group(1))
        self.assertLess(gpu, 1.0, 'the two servers share one card: their memory shares must stay below the whole card')

    def test_the_model_servers_have_no_route_out_and_only_the_download_job_does(self):
        for name in ('vllm', 'vllm-vision'):
            self.assertRegex(self.svc[name], r'networks: \[data, reader\]', name)
            self.assertNotRegex(self.svc[name], r'networks:[^\n]*(edge|egress)', name)
        self.assertIn('networks: [egress]', self.svc['vllm-fetch'])
        on_egress = {n for n, b in self.svc.items() if re.search(r'networks:[^\n]*egress', b)}
        # the worker is on it too (the timestamp authority); what matters is that no model server is, and that the download job is
        self.assertEqual(on_egress, {'worker', 'vllm-fetch'}, 'the download job is the only model container allowed to reach the internet')
        self.assertTrue((DEPLOY / 'fetch_model.py').exists())
        self.assertIn('./fetch_model.py:/fetch_model.py:ro', self.svc['vllm-fetch'])
        self.assertIn('restart: "no"', self.svc['vllm-fetch'])
        gpu = (DEPLOY / 'compose.gpu.yml').read_text(encoding='utf-8')
        for name in ('vllm', 'vllm-vision'):
            self.assertIn(f'  {name}:', gpu, name)
        self.assertNotIn('ollama', gpu)

    def test_the_application_asks_the_vllm_server_with_its_key(self):
        head = self.text.split('\nservices:\n', 1)[0]
        for needle in ('VLLM_BASE_URL: ${VLLM_BASE_URL:-http://vllm:8000}', 'VLLM_MODEL:', 'VLLM_API_KEY: ${VLLM_API_KEY:-}'):
            self.assertIn(needle, head)
        self.assertNotIn('OLLAMA', self.text)
        self.assertIn('VLM_API_KEY: ${VLLM_API_KEY:-}', self.svc['reader'])
        example = (ROOT / 'deploy' / 'env.production.example').read_text(encoding='utf-8')
        self.assertIn('QUEUE_AI_PROVIDER=vllm', example)
        self.assertIn('VLLM_API_KEY=', example)

    def test_the_installer_downloads_the_weights_first_then_starts_the_servers_and_never_pulls_with_ollama(self):
        sh = (DEPLOY / 'install.sh').read_text(encoding='utf-8')
        self.assertNotIn('ollama', sh.lower())
        self.assertIn('QUEUE_AI_PROVIDER=vllm', sh)
        self.assertLess(sh.index('vllm-fetch'), sh.index('$COMPOSE up -d --build'), 'the weights are fetched before the servers start offline')
        self.assertIn('--profile ai-fetch', sh)
        self.assertIn('HF_TOKEN', sh)
        self.assertIn('wait_healthy vllm', sh)

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
            if line.startswith(('claimguard-rs', 'claimguard-reader', 'claimguard-embed')):
                continue                                   # our own images, built from the Dockerfiles
            self.assertRegex(line, self.PIN, line)
        for df in (DEPLOY / 'Dockerfile', DEPLOY.parent / 'ocr' / 'Dockerfile', DEPLOY.parent / 'embed' / 'Dockerfile'):
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


class ProxyBodyLimits(unittest.TestCase):
    """Every proxy config must let through what the application accepts: 4 MB to submit a batch of 1,000 claims, 17 MB for an import, 100 KB elsewhere."""

    def test_the_caddyfiles_and_the_nginx_config_carry_the_same_three_limits(self):
        for name in ('Caddyfile.internal', 'Caddyfile.public'):
            text = (DEPLOY.parent / name).read_text(encoding='utf-8')
            self.assertRegex(text, r'@submit path /api/v1/queue/submit\s+handle @submit \{\s+request_body \{\s+max_size 4MB', name)
            self.assertIn('max_size 17MB', text, name)
        conf = (DEPLOY.parent / 'nginx' / 'nginx.conf').read_text(encoding='utf-8')
        self.assertIn('client_max_body_size 100k;', conf)
        self.assertRegex(conf, r'location = /api/v1/queue/submit \{\s+client_max_body_size 4m;')
        self.assertRegex(conf, r'import/\(file\|csv\)\$ \{\s+client_max_body_size 17m;')

    def test_the_limits_fit_the_application_and_the_client_address_is_overwritten(self):
        api = (DEPLOY.parents[1] / 'rust' / 'crates' / 'cg-api' / 'src' / 'lib.rs').read_text(encoding='utf-8')
        sub = eval(re.search(r'MAX_SUBMIT_BYTES: usize = ([\d\s*]+);', api).group(1))
        self.assertEqual(sub, 4 * 1024 * 1024)
        conf = (DEPLOY.parent / 'nginx' / 'nginx.conf').read_text(encoding='utf-8')
        # the application takes the LAST X-Forwarded-For hop as the client: nginx must overwrite what the client sent, never append to it
        self.assertIn('proxy_set_header X-Forwarded-For $remote_addr;', conf)
        self.assertNotIn('$proxy_add_x_forwarded_for', conf)
        # the event stream is never buffered or compressed
        self.assertRegex(conf, r'location = /api/v1/work/events \{[^}]*proxy_buffering off;[^}]*gzip off;')


class RustInstall(unittest.TestCase):
    def test_install_never_overwrites_an_existing_env_and_runs_the_rust_tools(self):
        text = (DEPLOY / 'install.sh').read_text(encoding='utf-8')
        self.assertIn('set -euo pipefail', text)
        self.assertRegex(text, r'if \[ -e \.env \]')
        self.assertIn('umask 077', text)
        self.assertIn('deploy/rust/compose.yml', text)
        self.assertIn('cg admin create-admin', text)
        # the document reader is a Python image by design (docs/45); its health probe is the one allowed use
        self.assertNotIn('python', text.lower().replace('# ', ' ').replace('exec -t reader python -c', ''))
        self.assertNotIn('deploy/compose.yml', text.replace('deploy/rust/compose.yml', ''))


if __name__ == '__main__':
    unittest.main()
