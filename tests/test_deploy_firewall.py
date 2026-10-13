"""The host firewall script (deploy/rust/firewall.sh) and the outside port check (deploy/rust/portcheck.py).

The script is exercised in --dry-run, which prints every iptables command instead of running it; rust/tools/firewall_test.py applies it for
real inside a throwaway Linux container. Neither was run on the target server."""
import importlib.util
import os
import re
import shutil
import socket
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEPLOY = ROOT / 'deploy' / 'rust'
SCRIPT = DEPLOY / 'firewall.sh'
BASH = shutil.which('bash')


def load_portcheck():
    spec = importlib.util.spec_from_file_location('portcheck', DEPLOY / 'portcheck.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def dry(args=('apply', '--dry-run'), **env):
    e = {k: v for k, v in os.environ.items() if not k.startswith('CG_')}
    e.update(env)
    p = subprocess.run([BASH, str(SCRIPT), *args], capture_output=True, text=True, env=e, timeout=60)
    return p.returncode, p.stdout, p.stderr


@unittest.skipUnless(BASH, 'bash is not available')
class FirewallDryRun(unittest.TestCase):
    def test_script_has_unix_line_endings_and_parses(self):
        self.assertNotIn(b'\r', SCRIPT.read_bytes())
        self.assertEqual(subprocess.run([BASH, '-n', str(SCRIPT)], capture_output=True).returncode, 0)

    def test_refuses_to_run_without_saying_where_ssh_comes_from(self):
        code, out, err = dry()
        self.assertNotEqual(code, 0)
        self.assertIn('CG_SSH_FROM', err)
        self.assertNotIn('iptables', out)

    def test_rejects_anything_that_is_not_an_address(self):
        for bad in ('1.2.3.4; rm -rf /', '300.1.1.1', '1.2.3.4/33', '$(id)', '1.2.3', 'example.org', '1.2.3.4,,evil', '`id`', 'zz:zz::1'):
            code, out, err = dry(CG_SSH_FROM=bad)
            self.assertNotEqual(code, 0, bad)
            self.assertNotIn('+ iptables', out, bad)

    def test_rejects_a_bad_port_or_interface(self):
        self.assertNotEqual(dry(CG_SSH_FROM='1.2.3.4', CG_SSH_PORT='0')[0], 0)
        self.assertNotEqual(dry(CG_SSH_FROM='1.2.3.4', CG_SSH_PORT='22; reboot')[0], 0)
        self.assertNotEqual(dry(CG_SSH_FROM='1.2.3.4', CG_EXT_IF='eth0; reboot')[0], 0)
        self.assertNotEqual(dry(('apply', '--dry-run', '--auto-revert', 'soon'), CG_SSH_FROM='1.2.3.4')[0], 0)

    def test_the_rules_are_the_intended_ones(self):
        code, out, err = dry(CG_SSH_FROM='203.0.113.7/32,198.51.100.0/24', CG_EXT_IF='ens3')
        self.assertEqual(code, 0, err)
        v4 = [l[2:] for l in out.splitlines() if l.startswith('+ iptables ')]
        # containers: only what arrived for 80 or 443 passes, the rest from outside is dropped, and the jump is FIRST in DOCKER-USER
        fw = [l for l in v4 if 'CLAIMGUARD-FW' in l and l.startswith('iptables -A')]
        self.assertEqual([re.search(r'--ctorigdstport (\d+)', l).group(1) for l in fw if 'ctorigdstport' in l], ['80', '443'])
        self.assertIn('iptables -A CLAIMGUARD-FW -i ens3 -j DROP', v4)
        self.assertIn('iptables -I DOCKER-USER 1 -j CLAIMGUARD-FW', v4)
        # the host: ends in a DROP, jump is first in INPUT
        self.assertEqual(v4[[i for i, l in enumerate(v4) if l.startswith('iptables -A CLAIMGUARD-IN')][-1]], 'iptables -A CLAIMGUARD-IN -j DROP')
        self.assertIn('iptables -I INPUT 1 -j CLAIMGUARD-IN', v4)
        accepted = [l for l in v4 if l.startswith('iptables -A CLAIMGUARD-IN') and l.endswith('-j ACCEPT') and '--dport' in l and 'sport' not in l]
        ports = sorted((re.search(r'--dport (\d+)', l).group(1), (re.search(r'-s (\S+)', l) or [None, None])[1]) for l in accepted)
        self.assertEqual(ports, [('22', '198.51.100.0/24'), ('22', '203.0.113.7/32'), ('443', None), ('80', None)])
        # no database, cache, model server or API port is ever opened
        for port in ('27017', '6379', '11434', '8443'):
            self.assertNotIn('--dport ' + port, out)
            self.assertNotIn('ctorigdstport ' + port, out)

    def test_ssh_for_everyone_only_when_asked_and_with_a_warning(self):
        code, out, err = dry(CG_SSH_FROM='any')
        self.assertEqual(code, 0)
        self.assertIn('WARNING', out)
        self.assertIn('iptables -A CLAIMGUARD-IN -p tcp --dport 22 -j ACCEPT', out)

    def test_ipv6_keeps_neighbour_discovery_and_only_v6_sources(self):
        code, out, _ = dry(CG_SSH_FROM='203.0.113.7/32,2001:db8::/32')
        v6 = [l for l in out.splitlines() if l.startswith('+ ip6tables ')]
        self.assertTrue(any('ipv6-icmp' in l for l in v6))
        self.assertTrue(any('-s 2001:db8::/32' in l for l in v6))
        self.assertFalse(any('203.0.113.7' in l for l in v6))
        self.assertFalse(any('203.0.113.7' in l and 'ip6tables' in l for l in out.splitlines()))

    def test_remove_and_persist_dry_runs_do_not_touch_anything(self):
        code, out, _ = dry(('remove', '--dry-run'))
        self.assertEqual(code, 0)
        self.assertIn('iptables -D DOCKER-USER -j CLAIMGUARD-FW', out)
        self.assertIn('iptables -X CLAIMGUARD-IN', out)
        code, out, _ = dry(('persist', '--dry-run'), CG_SSH_FROM='203.0.113.7/32')
        self.assertEqual(code, 0)
        self.assertIn('systemctl enable', out)

    def test_the_persistent_unit_waits_for_docker_and_the_env_file_is_private(self):
        text = SCRIPT.read_text(encoding='utf-8')
        self.assertRegex(text, r'After=docker\.service')
        self.assertRegex(text, r'Requires=docker\.service')
        self.assertIn('umask 077', text)


class PortCheck(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pc = load_portcheck()

    def test_port_lists(self):
        self.assertEqual(self.pc.parse_ports('80,443'), {80, 443})
        self.assertEqual(self.pc.parse_ports('1-3,8443'), {1, 2, 3, 8443})
        for bad in ('0', '70000', '5-2', 'x'):
            with self.assertRaises(ValueError):
                self.pc.parse_ports(bad)

    def test_judge(self):
        res = {80: 'open', 443: 'open', 6379: 'filtered', 27017: 'closed', 8443: 'open'}
        problems = self.pc.judge(res, {80, 443})
        self.assertEqual(len(problems), 1)
        self.assertIn('8443', problems[0])
        self.assertIn('claimguard api', problems[0])
        self.assertEqual(self.pc.judge({80: 'open', 443: 'open'}, {80, 443}), [])
        self.assertTrue(self.pc.judge({80: 'open', 443: 'filtered'}, {80, 443}))      # an expected port that does not answer is a failure too
        self.assertTrue(self.pc.judge({80: 'open'}, {80, 443}))                       # ... and so is one that was not scanned at all

    def _listener(self):
        s = socket.socket()
        s.bind(('127.0.0.1', 0))
        s.listen(5)
        self.addCleanup(s.close)
        return s.getsockname()[1]

    def _free_port(self):
        s = socket.socket()
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def test_real_sockets_open_and_closed(self):
        opened, closed = self._listener(), self._free_port()
        res = self.pc.scan('127.0.0.1', {opened, closed}, timeout=6.0)     # Windows reports a refused loopback connection only after ~2 s
        self.assertEqual(res[opened], 'open')
        self.assertEqual(res[closed], 'closed')

    def test_a_silent_port_is_filtered_not_open(self):
        fake = lambda h, p, t: 'filtered'
        self.assertEqual(self.pc.scan('x', {1, 2}, prober=fake), {1: 'filtered', 2: 'filtered'})
        self.assertEqual(self.pc.judge({1: 'filtered', 2: 'filtered'}, set()), [])

    def test_main_exit_codes(self):
        opened, closed = self._listener(), self._free_port()
        ok = self.pc.main(['127.0.0.1', '--ports', '%d,%d' % (opened, closed), '--web', str(opened), '--ssh-port', str(closed), '--timeout', '6'])
        self.assertEqual(ok, 0)                                                  # exactly the expected port is open
        self.assertEqual(self.pc.main(['127.0.0.1', '--ports', str(opened), '--web', str(self._free_port()), '--timeout', '6']), 1)   # unexpected open + expected closed
        self.assertEqual(self.pc.main(['127.0.0.1', '--ports', 'abc']), 2)
        self.assertEqual(self.pc.main(['no-such-host.invalid', '--ports', '80']), 2)


if __name__ == '__main__':
    unittest.main()
