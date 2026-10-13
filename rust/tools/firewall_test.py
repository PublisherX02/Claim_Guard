#!/usr/bin/env python3
"""Apply deploy/rust/firewall.sh FOR REAL in throwaway Linux containers and prove what it does to a published port.

Three containers reproduce how Docker publishes ports:
  client  (outside network)  --scans-->  router  (outside + inside network; port forwarding like Docker's; the script runs here)  --> backend
The router DNATs ports 80, 443 AND 6379 and 11434 to the backend, as if a compose file had published the database and the model server by
mistake, and listens on 22 itself. Expected:
  before the script   6379 and 11434 answer from outside (the Docker-versus-ufw trap), 22 answers
  after               only 80 and 443 answer; SSH answers only when the client's address is in CG_SSH_FROM
  twice in a row      one jump rule only (idempotent)
  (not tested: cutting of connections that are already open during a re-apply; the first rule of both chains accepts ESTABLISHED,RELATED)
  remove              back to the first state, chains gone
Needs Docker (Linux containers) and network access for the first image build. Does not touch the host's firewall. Exit 0 = all checks passed.

    python rust/tools/firewall_test.py
"""
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEPLOY = ROOT / 'deploy' / 'rust'
IMAGE = 'cgfw-test:1'
OUT, INN = 'cgfw-out', 'cgfw-in'
NAMES = ('cgfw-client', 'cgfw-router', 'cgfw-backend')
DOCKERFILE = """FROM python:3.12-slim
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends iptables iproute2 procps >/dev/null && rm -rf /var/lib/apt/lists/*
"""
LISTENER = ("import socket,sys,threading\n"
            "for p in map(int,sys.argv[1:]):\n"
            "    s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('0.0.0.0',p)); s.listen(50)\n"
            "    threading.Thread(target=lambda s=s:[s.accept()[0].close() for _ in iter(int,1)],daemon=True).start()\n"
            "threading.Event().wait()\n")

failures = []


def sh(*args, check=True, input=None):
    p = subprocess.run(args, capture_output=True, text=True, input=input)
    if check and p.returncode != 0:
        raise RuntimeError('%s failed (%d): %s%s' % (' '.join(args[:6]), p.returncode, p.stdout[-400:], p.stderr[-400:]))
    return p


def ex(name, cmd, check=True, env=None):
    args = ['docker', 'exec']
    for k, v in (env or {}).items():
        args += ['-e', '%s=%s' % (k, v)]
    return sh(*args, name, 'bash', '-c', cmd, check=check)


def check(label, ok, detail=''):
    print(('PASS  ' if ok else 'FAIL  ') + label + ((' -- ' + detail) if detail and not ok else ''))
    if not ok:
        failures.append(label)


def ip_of(name, net):
    return sh('docker', 'inspect', '-f', '{{(index .NetworkSettings.Networks "%s").IPAddress}}' % net, name).stdout.strip()


def cleanup():
    for n in NAMES:
        sh('docker', 'rm', '-f', n, check=False)
    for n in (OUT, INN):
        sh('docker', 'network', 'rm', n, check=False)


def scan(ssh_state):
    """run portcheck.py inside the client against the router's outside address; -> (exit code, text)"""
    p = ex('cgfw-client', 'python3 /portcheck.py %s --ports 1-1024,2375,6379,8443,11434,27017 --ssh %s --timeout 1' % (ROUTER_OUT, ssh_state), check=False)
    return p.returncode, p.stdout + p.stderr


def open_ports(text):
    m = re.search(r'^open: (.*)$', text, re.M)
    return sorted(int(x) for x in re.findall(r'\d+', m.group(1))) if m else None


def main():
    global ROUTER_OUT
    if sh('docker', 'version', '--format', '{{.Server.Os}}', check=False).stdout.strip() != 'linux':
        print('needs a Docker engine running Linux containers')
        return 2
    cleanup()
    try:
        if sh('docker', 'image', 'inspect', IMAGE, check=False).returncode != 0:
            print('building the test image (once)...')
            sh('docker', 'build', '-q', '-t', IMAGE, '-', input=DOCKERFILE)
        sh('docker', 'network', 'create', OUT)
        sh('docker', 'network', 'create', '--internal', INN)
        sh('docker', 'run', '-d', '--name', 'cgfw-backend', '--network', INN, IMAGE, 'python3', '-c', LISTENER, '80', '443', '6379', '11434')
        sh('docker', 'run', '-d', '--name', 'cgfw-router', '--network', OUT, '--cap-add', 'NET_ADMIN', '--sysctl', 'net.ipv4.ip_forward=1', IMAGE,
           'python3', '-c', LISTENER, '22')
        sh('docker', 'network', 'connect', INN, 'cgfw-router')
        sh('docker', 'run', '-d', '--name', 'cgfw-client', '--network', OUT, IMAGE, 'sleep', '3600')
        sh('docker', 'cp', str(DEPLOY / 'portcheck.py'), 'cgfw-client:/portcheck.py')
        sh('docker', 'cp', str(DEPLOY / 'firewall.sh'), 'cgfw-router:/firewall.sh')
        time.sleep(1)
        ROUTER_OUT, CLIENT, BACKEND = ip_of('cgfw-router', OUT), ip_of('cgfw-client', OUT), ip_of('cgfw-backend', INN)
        router_in = ip_of('cgfw-router', INN)
        ext_if = ex('cgfw-router', "ip -o -4 addr show | awk '/%s/{print $2}'" % ROUTER_OUT).stdout.strip()
        int_if = ex('cgfw-router', "ip -o -4 addr show | awk '/%s/{print $2}'" % router_in).stdout.strip()
        print('router outside %s (%s), inside %s (%s); client %s; backend %s' % (ROUTER_OUT, ext_if, router_in, int_if, CLIENT, BACKEND))
        # Docker's own plumbing, reproduced: published ports are DNAT rules, DOCKER-USER sits at the top of FORWARD
        ex('cgfw-router', ' && '.join([
            'iptables -N DOCKER-USER', 'iptables -A DOCKER-USER -j RETURN', 'iptables -I FORWARD 1 -j DOCKER-USER',
            'iptables -A FORWARD -j ACCEPT',
            *['iptables -t nat -A PREROUTING -i %s -p tcp --dport %d -j DNAT --to-destination %s:%d' % (ext_if, p, BACKEND, p) for p in (80, 443, 6379, 11434)],
            'iptables -t nat -A POSTROUTING -o %s -j MASQUERADE' % int_if]))

        rc, text = scan('closed')
        check('before: the scan reaches the published database and model ports', open_ports(text) == [22, 80, 443, 6379, 11434], text)

        env = {'CG_SSH_FROM': '203.0.113.7/32', 'CG_EXT_IF': ext_if}
        p = ex('cgfw-router', 'bash /firewall.sh apply', env=env, check=False)
        check('apply succeeds on a real kernel', p.returncode == 0, p.stdout + p.stderr)
        rc, text = scan('closed')
        check('after apply: only 80 and 443 answer from outside', open_ports(text) == [80, 443] and rc == 0, text)
        check('after apply: the published database port 6379 and model port 11434 are silent (filtered, not refused)',
              'filtered' in text and 6379 not in (open_ports(text) or []) and 11434 not in (open_ports(text) or []), text)

        env['CG_SSH_FROM'] = '203.0.113.7/32,%s/32' % CLIENT
        p = ex('cgfw-router', 'bash /firewall.sh apply', env=env, check=False)
        check('apply a second time (idempotent)', p.returncode == 0, p.stdout + p.stderr)
        jumps = ex('cgfw-router', 'iptables -S DOCKER-USER; iptables -S INPUT').stdout
        check('exactly one jump into each chain after two applies', jumps.count('-j CLAIMGUARD-FW') == 1 and jumps.count('-j CLAIMGUARD-IN') == 1, jumps)
        first = ex('cgfw-router', 'iptables -S DOCKER-USER').stdout.splitlines()
        check('the jump is the first rule of DOCKER-USER', len(first) > 1 and first[1].endswith('-j CLAIMGUARD-FW'), '\n'.join(first))
        rc, text = scan('open')
        check('trusted address: SSH answers as well, still nothing else', open_ports(text) == [22, 80, 443] and rc == 0, text)
        rc, text = scan('closed')
        check('the port check notices SSH when it should be closed (exit 1)', rc == 1 and 'port 22' in text, text)

        p = ex('cgfw-router', 'bash /firewall.sh status', check=False)
        check('status prints both chains', 'CLAIMGUARD-FW' in p.stdout and 'CLAIMGUARD-IN' in p.stdout and p.returncode == 0, p.stdout + p.stderr)

        p = ex('cgfw-router', 'bash /firewall.sh remove', env=env, check=False)
        check('remove succeeds', p.returncode == 0, p.stdout + p.stderr)
        left = ex('cgfw-router', 'iptables -S | grep -c CLAIMGUARD || true').stdout.strip()
        check('remove leaves no chain and no jump behind', left == '0', left)
        rc, text = scan('closed')
        check('after remove: the original (open) state is back', open_ports(text) == [22, 80, 443, 6379, 11434], text)

        # the safety valve: apply with --auto-revert and give no answer -> the rules go away again
        p = ex('cgfw-router', 'bash /firewall.sh apply --auto-revert 3 < /dev/null', env=env, check=False)
        left = ex('cgfw-router', 'iptables -S | grep -c CLAIMGUARD || true').stdout.strip()
        check('--auto-revert without a "yes" removes the rules', p.returncode == 0 and left == '0' and 'removing the rules' in p.stdout, p.stdout + left)
    finally:
        cleanup()
    print()
    print('FAILED: %d (%s)' % (len(failures), ', '.join(failures)) if failures else 'ALL CHECKS PASSED')
    return 1 if failures else 0


ROUTER_OUT = ''
if __name__ == '__main__':
    sys.exit(main())
