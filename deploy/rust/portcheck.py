#!/usr/bin/env python3
"""Scan a ClaimGuard server's TCP ports FROM OUTSIDE and fail if anything other than the expected ports answers.

    python3 deploy/rust/portcheck.py 203.0.113.10                      # run from a machine that is NOT allowed to use SSH: 80 and 443 only
    python3 deploy/rust/portcheck.py 203.0.113.10 --ssh open           # run from the trusted administration address: SSH must answer too
    python3 deploy/rust/portcheck.py 203.0.113.10 --ports 1-65535      # every port (slow when packets are dropped silently)

Run it from ANOTHER machine, never on the server itself: a packet the server sends to its own address travels over the loopback interface,
which the firewall always lets through, so a scan from the server proves nothing.

Result per port: open (a connection was made), closed (refused), filtered (no answer within the timeout). Only "open" counts against you; a
dropped port shows as filtered, which is what the firewall is meant to produce. UDP cannot be scanned this way (an open UDP port stays silent);
the firewall drops all UDP to the containers and the host is checked only by reading `firewall.sh status`.
Exit status: 0 = exactly the expected ports are open, 1 = something unexpected is open or an expected port is not, 2 = bad usage.
"""
import argparse
import socket
import sys
from concurrent.futures import ThreadPoolExecutor

# Services this stack runs or commonly sits next to; probed in addition to 1-1024 by default, so a mistake shows up by name.
NAMED = {
    22: 'ssh', 23: 'telnet', 25: 'smtp', 80: 'http', 111: 'rpcbind', 135: 'rpc', 139: 'netbios', 443: 'https', 445: 'smb', 2375: 'docker (plain)',
    2376: 'docker (tls)', 3000: 'dev server', 3306: 'mysql', 3389: 'rdp', 5432: 'postgres', 5900: 'vnc', 6379: 'redis', 8000: 'dev server',
    8080: 'http alt', 8443: 'claimguard api', 9200: 'elasticsearch', 11434: 'ollama', 27017: 'mongodb', 27018: 'mongodb alt',
}


def parse_ports(text):
    ports = set()
    for part in text.split(','):
        part = part.strip()
        if not part:
            continue
        lo, _, hi = part.partition('-')
        a, b = int(lo), int(hi or lo)
        if not (1 <= a <= b <= 65535):
            raise ValueError(part)
        ports.update(range(a, b + 1))
    return ports


def probe(host, port, timeout):
    """open | closed | filtered"""
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return 'open'
    except ConnectionRefusedError:
        return 'closed'
    except (socket.timeout, TimeoutError):
        return 'filtered'
    except OSError:
        return 'filtered'


def scan(host, ports, timeout=1.5, workers=256, prober=probe):
    ports = sorted(ports)
    with ThreadPoolExecutor(max_workers=workers) as pool:
        return dict(zip(ports, pool.map(lambda p: prober(host, p, timeout), ports)))


def judge(results, expected_open):
    """-> (problems, ok_lines)"""
    problems = []
    for port, state in sorted(results.items()):
        if state == 'open' and port not in expected_open:
            problems.append('port %d (%s) is OPEN and should not be' % (port, NAMED.get(port, 'unknown service')))
    for port in sorted(expected_open):
        if results.get(port) != 'open':
            problems.append('port %d (%s) should answer but is %s' % (port, NAMED.get(port, 'expected'), results.get(port, 'not scanned')))
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('host')
    ap.add_argument('--ports', default=None, help='ports to scan, e.g. 1-1024,8443 (default: 1-1024 plus the named services above)')
    ap.add_argument('--ssh', choices=('open', 'closed'), default='closed',
                    help='is SSH expected to answer from where you run this? (default closed: you are an outsider)')
    ap.add_argument('--ssh-port', type=int, default=22)
    ap.add_argument('--web', default='80,443', help='ports that must answer (default 80,443)')
    ap.add_argument('--timeout', type=float, default=1.5)
    args = ap.parse_args(argv)
    try:
        ports = (parse_ports('1-1024') | set(NAMED)) if args.ports is None else parse_ports(args.ports)
        ports.add(args.ssh_port)
        expected = parse_ports(args.web)
    except ValueError as e:
        print('bad port list: %s' % e, file=sys.stderr)
        return 2
    if args.ssh == 'open':
        expected.add(args.ssh_port)
    ports |= expected
    try:
        socket.getaddrinfo(args.host, None)
    except OSError as e:
        print('cannot resolve %s: %s' % (args.host, e), file=sys.stderr)
        return 2
    if args.host.startswith('127.') or args.host in ('localhost', '::1'):
        print('WARNING: scanning the loopback address proves nothing about the firewall; scan the server\'s public address from another machine.')
    print('scanning %d ports on %s (timeout %.1fs)...' % (len(ports), args.host, args.timeout))
    results = scan(args.host, ports, args.timeout)
    counts = {s: sum(1 for v in results.values() if v == s) for s in ('open', 'closed', 'filtered')}
    print('open: %s' % (sorted(p for p, s in results.items() if s == 'open') or 'none'))
    print('closed (refused): %d, filtered (silent): %d' % (counts['closed'], counts['filtered']))
    problems = judge(results, expected)
    for p in problems:
        print('FAIL: ' + p)
    if problems:
        return 1
    print('PASS: exactly %s answer(s), nothing else open in the scanned range.' % sorted(expected))
    return 0


if __name__ == '__main__':
    sys.exit(main())
