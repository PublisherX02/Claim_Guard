"""Black-box test of the live-update stream (GET /api/v1/work/events) against a demo server this script starts itself.

    python rust/tools/sse_test.py

Checks: no stream without a session or without the permission, a bad topic is refused, the stream starts with "hello", stays quiet while
nothing changes, says "changed" after the caller's inbox changes, carries no claim data (every line is one of a fixed few), the queue topic
follows the counts for an administrator, a signed-out session ends its stream with "end" and a completed response, and the per-address limit refuses the stream after
it and frees the slots when streams close. Authorized scope: the user's own application on 127.0.0.1 with generated demo accounts.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pentest  # noqa: E402  (its Session logs in with the generated demo accounts)

ROOT = Path(__file__).resolve().parents[2]
PORT = 8493
pentest.PORT = PORT
HOST = '127.0.0.1'
FAILED = []
ALLOWED = ('retry: 3000', 'event: hello', 'event: changed', 'event: end', 'data: {}', 'data: {"reason": "session"}', ': keep-alive', '')


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + (('  -- ' + str(detail)[:300]) if detail and not ok else ''), flush=True)
    if not ok:
        FAILED.append(name)


class Stream:
    """A raw-socket reader of one event stream: status, headers, and the lines received so far."""

    def __init__(self, path, cookie=None):
        self.lines, self.status, self.headers, self.closed, self.raw = [], None, {}, False, b''
        self.sock = socket.create_connection((HOST, PORT), timeout=10)
        self.sock.settimeout(None)   # a stream is quiet for seconds at a time
        head = 'GET %s HTTP/1.1\r\nHost: %s:%d\r\nAccept: text/event-stream\r\n' % (path, HOST, PORT)
        if cookie:
            head += 'Cookie: cg_session=%s\r\n' % cookie
        self.sock.sendall((head + '\r\n').encode())
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()
        self.wait(lambda: self.status is not None or self.closed, 5)

    def _read(self):
        buf = b''
        try:
            while True:
                chunk = self.sock.recv(65536)
                if not chunk:
                    break
                buf += chunk
                self.raw += chunk
                if self.status is None and b'\r\n\r\n' in buf:
                    head, _, buf = buf.partition(b'\r\n\r\n')
                    lines = head.decode('latin1').split('\r\n')
                    self.status = int(lines[0].split()[1])
                    self.headers = {k.strip().lower(): v.strip() for k, _, v in (l.partition(':') for l in lines[1:])}
                if self.status is not None:
                    text = buf.decode('utf-8', 'replace')
                    if self.headers.get('transfer-encoding') == 'chunked':
                        # drop chunk-size lines (hex numbers on their own line)
                        text = '\n'.join(l for l in text.replace('\r\n', '\n').split('\n') if not (l and all(c in '0123456789abcdefABCDEF' for c in l) and len(l) <= 4))
                    self.lines = [l.rstrip('\r') for l in text.replace('\r\n', '\n').split('\n')]
        except OSError:
            pass
        self.closed = True

    def wait(self, pred, timeout):
        end = time.time() + timeout
        while time.time() < end:
            if pred():
                return True
            time.sleep(0.1)
        return pred()

    def has(self, line):
        return line in self.lines

    def count(self, line):
        return self.lines.count(line)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def main():
    data = Path(os.environ.get('TEMP', '/tmp')) / 'cg_sse'
    data.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CLAIMGUARD_ROOT=str(ROOT))
    proc = subprocess.Popen([pentest.CG, 'serve', '--demo', '--port', str(PORT), '--data-dir', str(data)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    streams = []
    try:
        for _ in range(80):
            try:
                urllib.request.urlopen('http://%s:%d/healthz' % (HOST, PORT), timeout=1)
                break
            except Exception:  # noqa: BLE001
                time.sleep(0.5)
        acc = json.loads((data / 'demo_accounts.json').read_text())
        time.sleep(4)   # let the demo worker deal the first inboxes
        # ---- who may open a stream
        s = Stream('/api/v1/work/events?topics=inbox')
        streams.append(s)
        check('no session: an ordinary 401, not a stream', s.status == 401 and 'event-stream' not in s.headers.get('content-type', ''), (s.status, s.headers))
        viewer = pentest.Session('CG-1001', acc['CG-1001'])
        s = Stream('/api/v1/work/events?topics=inbox', viewer.cookie)
        streams.append(s)
        check('a viewer (no claims.decide) gets 403', s.status == 403, s.status)
        rev = pentest.Session('CG-2002', acc['CG-2002'])
        rev2 = pentest.Session('CG-2003', acc['CG-2003'])
        for topics, want in (('queue', 403), ('bogus', 422), ('inbox,inbox', 422), ('inbox,queue', 403), ('', 422)):    # the API answers invalid input with 422
            s = Stream('/api/v1/work/events?topics=' + topics, rev.cookie)
            streams.append(s)
            check('reviewer, topics=%r -> %d' % (topics, want), s.status == want, s.status)
        # ---- the reviewer's inbox stream
        s = Stream('/api/v1/work/events?topics=inbox', rev.cookie)
        streams.append(s)
        check('reviewer inbox stream opens as text/event-stream', s.status == 200 and s.headers.get('content-type', '').startswith('text/event-stream'), (s.status, s.headers))
        check('no-store and no sniffing on the stream', s.headers.get('cache-control') == 'no-store' and s.headers.get('x-content-type-options') == 'nosniff', s.headers)
        check('it starts with hello', s.wait(lambda: s.has('event: hello'), 3), s.lines)
        other = Stream('/api/v1/work/events?topics=inbox', rev2.cookie)
        streams.append(other)
        other.wait(lambda: other.has('event: hello'), 3)
        admin = pentest.Session('CG-4004', acc['CG-4004'])
        qs = Stream('/api/v1/work/events?topics=queue', admin.cookie)
        streams.append(qs)
        check('admin queue stream opens with hello', qs.wait(lambda: qs.has('event: hello'), 3), (qs.status, qs.lines))
        time.sleep(7)
        quiet = s.count('event: changed')
        check('quiet while nothing changes (7 s)', quiet == 0, s.lines)
        # ---- a change: the reviewer clears a green claim (it leaves the inbox)
        st, _, inbox = rev.call('GET', '/api/v1/work/inbox')
        green = [c['claim_id'] for c in inbox['claims'] if c.get('lane') == 'green'] if st == 200 else []
        check('there is a green claim in the inbox to clear', bool(green), (st, str(inbox)[:200]))
        if green:
            st, _, body = rev.call('POST', '/api/v1/work/claims/%s/verify' % green[0], {'action': 'verify_clear'})
            check('clearing it succeeds', st == 200, (st, str(body)[:200]))
            check('the inbox stream says changed within 8 s', s.wait(lambda: s.count('event: changed') >= 1, 8), s.lines)
            check('the administrator\'s queue stream says changed too', qs.wait(lambda: qs.count('event: changed') >= 1, 8), qs.lines)
        # ---- no claim data on any stream, ever
        everything = s.lines + qs.lines + other.lines
        stray = [l for l in everything if l not in ALLOWED]
        check('every line on every stream is one of the fixed few (no claim data)', not stray, stray[:5])
        leaked = [c for c in (green[:1] if green else []) if c.encode() in s.raw + qs.raw + other.raw]
        check('the cleared claim id appears on no stream', not leaked)
        # ---- sign out ends the stream
        rev.call('POST', '/api/v1/auth/logout', {})
        check('after sign-out the stream says end within 8 s and its response is completed', s.wait(lambda: s.has('event: end') and s.raw.endswith(b'0' + bytes([13, 10, 13, 10])), 8), (s.lines, s.raw[-20:]))
        # ---- limits
        for held in (other, qs):     # free the slots this script still holds, so the count below is exact
            held.close()
        time.sleep(STREAM_FREE_WAIT)
        opened = [Stream('/api/v1/work/events?topics=inbox', rev2.cookie) for _ in range(16)]
        streams.extend(opened)
        check('16 streams from one address are accepted', all(o.status == 200 for o in opened), [o.status for o in opened])
        over = Stream('/api/v1/work/events?topics=inbox', rev2.cookie)
        streams.append(over)
        check('the 17th from the same address is refused with 429', over.status == 429, over.status)
        for o in opened:
            o.close()
        time.sleep(STREAM_FREE_WAIT)
        again = Stream('/api/v1/work/events?topics=inbox', rev2.cookie)
        streams.append(again)
        check('slots come back after the streams close', again.status == 200, again.status)
        # ---- the ordinary routes still answer while streams are open
        st, _, h = admin.call('GET', '/api/v1/queue/dashboard')
        check('the API still answers while streams are open', st == 200, st)
    finally:
        for s in streams:
            s.close()
        proc.kill()
    print('\n%s' % ('FAILED: %d (%s)' % (len(FAILED), '; '.join(FAILED)) if FAILED else 'ALL CHECKS PASSED'))
    return 1 if FAILED else 0


STREAM_FREE_WAIT = 8   # a closed stream frees its slot at its next tick (3 s); wait two ticks


if __name__ == '__main__':
    sys.exit(main())
