"""Black-box test of the WebSocket live-update endpoint (GET /api/v1/work/ws) against a demo server this script starts itself.

    python rust/tools/ws_test.py

Checks: no socket without a session, without the permission or with a bad topic (an ordinary error, no upgrade); a bad handshake is refused; a socket
whose Origin is another site is refused and the same site's is accepted (the cross-site WebSocket hijacking check); the handshake answer carries the right
Sec-WebSocket-Accept; the first message is hello; the socket stays quiet while nothing changes; it says changed after the caller's inbox changes and after the
queue changes for an administrator; every message is one of a fixed few (no claim data); a ping is answered with a pong; an unmasked client frame and a
large client message are closed with 1002 and 1009; a client close is answered; signing out ends the socket with "end" and a close; the per-address limit
is shared with the event streams and the slots come back. Authorized scope: the user's own application on 127.0.0.1 with generated demo accounts.
"""
import base64
import json
import os
import socket
import struct
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402  (its Session logs in with the generated demo accounts)

ROOT = Path(__file__).resolve().parents[2]
PORT = 8494
pentest.PORT = PORT
HOST = '127.0.0.1'
FAILED = []
ALLOWED = ('{"event":"hello"}', '{"event":"changed"}', '{"event":"end"}')
KEY = base64.b64encode(os.urandom(16)).decode()


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


def frame(opcode, payload=b'', mask=True, fin=True):
    head = bytes([(0x80 if fin else 0) | opcode])
    n = len(payload)
    lenbytes = bytes([(0x80 if mask else 0) | n]) if n < 126 else bytes([(0x80 if mask else 0) | 126]) + struct.pack('>H', n)
    if not mask:
        return head + lenbytes + payload
    m = os.urandom(4)
    return head + lenbytes + m + bytes(b ^ m[i % 4] for i, b in enumerate(payload))


class Sock:
    """A raw-socket WebSocket client: the handshake answer, then the server's frames."""

    def __init__(self, path, cookie=None, origin='same', key=KEY, extra=''):
        self.s = socket.create_connection((HOST, PORT), timeout=3)
        o = {'same': 'Origin: http://%s:%d\r\n' % (HOST, PORT), 'none': ''}.get(origin, 'Origin: %s\r\n' % origin)
        req = ('GET %s HTTP/1.1\r\nHost: %s:%d\r\n%sUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n%s%s\r\n'
               % (path, HOST, PORT, o, ('Sec-WebSocket-Key: %s\r\n' % key) if key else '', ('Cookie: cg_session=%s\r\n' % cookie) if cookie else '') + extra)
        self.s.sendall(req.encode())
        self.buf = b''
        while b'\r\n\r\n' not in self.buf:
            d = self.s.recv(4096)
            if not d:
                break
            self.buf += d
        head, _, self.buf = self.buf.partition(b'\r\n\r\n')
        lines = head.decode('latin1').split('\r\n')
        self.status = int(lines[0].split()[1]) if lines and lines[0] else 0
        self.headers = {l.split(':', 1)[0].strip().lower(): l.split(':', 1)[1].strip() for l in lines[1:] if ':' in l}
        self.texts, self.closes, self.pings, self.pongs = [], [], 0, []

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            self._parse()
            self.s.settimeout(max(0.05, min(0.3, end - time.time())))
            try:
                d = self.s.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                return
            if not d:
                self.eof = True
                self._parse()
                return
            self.buf += d
        self._parse()

    def _parse(self):
        while len(self.buf) >= 2:
            op, n = self.buf[0] & 0x0f, self.buf[1] & 0x7f
            if n >= 126 or len(self.buf) < 2 + n:
                return
            payload, self.buf = self.buf[2:2 + n], self.buf[2 + n:]
            if op == 1:
                self.texts.append(payload.decode())
            elif op == 8:
                self.closes.append(struct.unpack('>H', payload[:2])[0] if len(payload) >= 2 else 1005)
            elif op == 9:
                self.pings += 1
            elif op == 10:
                self.pongs.append(payload)

    def wait(self, cond, seconds):
        end = time.time() + seconds
        while time.time() < end:
            self.pump(0.3)
            if cond():
                return True
        return cond()

    def send(self, data):
        self.s.sendall(data)

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def main():
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_ws'
    data.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT))
    proc = subprocess.Popen([pentest.CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    socks = []

    def open_ws(*a, **k):
        w = Sock(*a, **k)
        socks.append(w)
        return w
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen('http://%s:%d/healthz' % (HOST, PORT), timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        time.sleep(4)   # let the demo worker deal the first inboxes
        P = '/api/v1/work/ws?topics='
        # ---- who may open a socket
        w = open_ws(P + 'inbox')
        check('no session: an ordinary 401, no upgrade', w.status == 401 and 'sec-websocket-accept' not in w.headers, (w.status, w.headers))
        viewer = pentest.Session('CG-1001', acc['CG-1001'])
        check('a viewer (no claims.decide) gets 403', open_ws(P + 'inbox', viewer.cookie).status == 403)
        rev = pentest.Session('CG-2002', acc['CG-2002'])
        rev2 = pentest.Session('CG-2003', acc['CG-2003'])
        for topics, want in (('queue', 403), ('bogus', 422), ('inbox,inbox', 422), ('', 422)):
            check('reviewer, topics=%r -> %d' % (topics, want), open_ws(P + topics, rev.cookie).status == want)
        # ---- the handshake
        check('a request without a key is a 400', open_ws(P + 'inbox', rev.cookie, key=None).status == 400)
        check('a key that is not 16 bytes is a 400', open_ws(P + 'inbox', rev.cookie, key=base64.b64encode(b'short').decode()).status == 400)
        for origin in ('https://evil.example', 'null', 'http://%s:%d.evil.example' % (HOST, PORT), 'http://%s:%d' % (HOST, PORT + 1)):
            check('Origin %r is refused with 403' % origin, open_ws(P + 'inbox', rev.cookie, origin=origin).status == 403)
        w = open_ws(P + 'inbox', rev.cookie)
        accept = base64.b64encode(__import__('hashlib').sha1((KEY + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        check('the same site\'s socket is accepted with 101 and the right accept key', w.status == 101 and w.headers.get('sec-websocket-accept') == accept and w.headers.get('upgrade', '').lower() == 'websocket', (w.status, w.headers))
        check('no-store on the handshake answer', w.headers.get('cache-control') == 'no-store', w.headers)
        check('it starts with hello', w.wait(lambda: '{"event":"hello"}' in w.texts, 3), w.texts)
        nonbrowser = open_ws(P + 'inbox', rev2.cookie, origin='none')
        check('a client without an Origin (not a page) is accepted', nonbrowser.status == 101)
        nonbrowser.wait(lambda: bool(nonbrowser.texts), 3)
        # ---- pings and violations
        w.send(frame(9, b'abc'))
        check('a ping is answered with a pong carrying the same bytes', w.wait(lambda: b'abc' in w.pongs, 3), w.pongs)
        bad = open_ws(P + 'inbox', rev2.cookie)
        bad.wait(lambda: bool(bad.texts), 3)
        bad.send(frame(1, b'hi', mask=False))
        check('an unmasked client frame is closed with 1002', bad.wait(lambda: 1002 in bad.closes, 3), bad.closes)
        big = open_ws(P + 'inbox', rev2.cookie)
        big.wait(lambda: bool(big.texts), 3)
        big.send(frame(1, b'x' * 2000))
        check('a large client message is closed with 1009', big.wait(lambda: 1009 in big.closes, 3), big.closes)
        polite = open_ws(P + 'inbox', rev2.cookie)
        polite.wait(lambda: bool(polite.texts), 3)
        polite.send(frame(8, struct.pack('>H', 1000)))
        check('a client close is answered with a close', polite.wait(lambda: 1000 in polite.closes, 3), polite.closes)
        # ---- changes
        admin = pentest.Session('CG-4004', acc['CG-4004'])
        qs = open_ws(P + 'queue', admin.cookie)
        check('admin queue socket opens with hello', qs.status == 101 and qs.wait(lambda: '{"event":"hello"}' in qs.texts, 3), (qs.status, qs.texts))
        time.sleep(7)
        w.pump(0.2)
        check('quiet while nothing changes (7 s)', w.texts.count('{"event":"changed"}') == 0, w.texts)
        st, _, inbox = rev.call('GET', '/api/v1/work/inbox')
        green = [c['claim_id'] for c in inbox['claims'] if c.get('lane') == 'green'] if st == 200 else []
        check('there is a green claim in the inbox to clear', bool(green), (st, str(inbox)[:200]))
        if green:
            st, _, body = rev.call('POST', '/api/v1/work/claims/%s/verify' % green[0], {'action': 'verify_clear'})
            check('clearing it succeeds', st == 200, (st, str(body)[:200]))
            check('the inbox socket says changed within 8 s', w.wait(lambda: '{"event":"changed"}' in w.texts, 8), w.texts)
            check('the administrator\'s queue socket says changed too', qs.wait(lambda: '{"event":"changed"}' in qs.texts, 8), qs.texts)
        # ---- no claim data on any socket, ever
        stray = [t for x in (w, qs, nonbrowser) for t in x.texts if t not in ALLOWED]
        check('every message on every socket is one of the fixed few (no claim data)', not stray, stray[:5])
        check('the cleared claim id appears on no socket', not any(green[0] in t for x in (w, qs, nonbrowser) for t in x.texts) if green else True)
        # ---- sign out ends the socket
        rev.call('POST', '/api/v1/auth/logout', {})
        check('after sign-out the socket says end and closes within 8 s', w.wait(lambda: '{"event":"end"}' in w.texts and bool(w.closes), 8), (w.texts, w.closes))
        # ---- limits (shared with the event streams)
        for held in (qs, nonbrowser, bad, big, polite):
            held.close()
        time.sleep(8)   # a closed socket frees its slot at the next tick (3 s); wait two ticks
        opened = [open_ws(P + 'inbox', rev2.cookie) for _ in range(16)]
        check('16 sockets from one address are accepted', all(o.status == 101 for o in opened), [o.status for o in opened])
        over = open_ws(P + 'inbox', rev2.cookie)
        check('the 17th from the same address is refused with 429', over.status == 429, over.status)
        for o in opened:
            o.close()
        time.sleep(8)
        again = open_ws(P + 'inbox', rev2.cookie)
        check('slots come back after the sockets close', again.status == 101, again.status)
        st, _, _h = admin.call('GET', '/api/v1/queue/dashboard')
        check('the API still answers while sockets are open', st == 200, st)
    finally:
        for x in socks:
            x.close()
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


if __name__ == '__main__':
    sys.exit(main())
