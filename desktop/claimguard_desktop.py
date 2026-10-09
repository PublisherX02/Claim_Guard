"""ClaimGuard for the desktop: the reviewer console in its own window.

    python desktop/claimguard_desktop.py --server https://claims.hospital.example      # a real server (remembered for next time)
    python desktop/claimguard_desktop.py --local-demo                                  # a private demo on this PC, nothing leaves it
    python desktop/claimguard_desktop.py --browser --server https://...                # no window toolkit: open the system browser

It is the same page the server serves to a browser (src/access/ui/), shown in a native window (Microsoft Edge WebView2 on Windows).
The window holds no data and no secret of its own: sign-in, the session cookie and every permission check are the server's. A remote
server must use https; plain http is accepted only for this PC (127.0.0.1, localhost). The local demo starts the real server (the Rust build) on a free
port bound to this PC only, with generated demo accounts that exist for the life of the window.
"""
import argparse
import atexit
import base64
import hashlib
import hmac
import http.client
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))

CONFIG_DIR = Path.home() / '.claimguard'
CONFIG_FILE = CONFIG_DIR / 'server.txt'
LOOPBACK = {'127.0.0.1', 'localhost', '::1'}
TITLE = 'ClaimGuard Console'


def clean_server_url(text):
    """The console's address from what a person typed, or ValueError. https is required unless the host is this PC."""
    text = (text or '').strip()
    if '://' not in text:
        text = 'https://' + text
    parts = urlparse(text)
    try:
        parts.port                                    # raises on a malformed port such as "javascript:alert(1)"
    except ValueError:
        raise ValueError('give the server as https://host[:port]') from None
    if parts.scheme not in ('http', 'https') or not parts.hostname or parts.username or parts.password or parts.path not in ('', '/', '/ui', '/ui/'):
        raise ValueError('give the server as https://host[:port]')
    if parts.scheme == 'http' and parts.hostname not in LOOPBACK:
        raise ValueError('a server on another machine must use https, because sign-in sends a password')
    return f'{parts.scheme}://{parts.netloc}'


def saved_server():
    try:
        return clean_server_url(CONFIG_FILE.read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return None


def remember_server(url):
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_FILE.write_text(url, encoding='utf-8')


def ask_server():
    """A small native prompt for the server address (the standard library's Tk), or None if cancelled."""
    import tkinter
    from tkinter import simpledialog
    root = tkinter.Tk()
    root.withdraw()
    try:
        return simpledialog.askstring(TITLE, 'Server address (for example https://claims.hospital.example):')
    finally:
        root.destroy()


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait_until_up(port, seconds=90):
    end = time.time() + seconds
    while time.time() < end:
        try:
            conn = http.client.HTTPConnection('127.0.0.1', port, timeout=2)
            conn.request('GET', '/healthz')
            if conn.getresponse().status == 200:
                return True
        except OSError:
            time.sleep(0.4)
    return False


def totp_now(secret, step=30, digits=6):
    """The current authenticator code for a base32 seed (RFC 6238, SHA-1), so the window needs no extra package."""
    key = base64.b32decode(secret.upper() + '=' * (-len(secret) % 8))
    mac = hmac.new(key, struct.pack('>Q', int(time.time()) // step), hashlib.sha1).digest()
    offset = mac[-1] & 15
    return str((struct.unpack('>I', mac[offset:offset + 4])[0] & 0x7FFFFFFF) % 10 ** digits).zfill(digits)


class DemoBridge:
    """Exposed to the page only in the local demo (as window.pywebview.api): hands the sign-in form a generated demo login."""

    def __init__(self, accounts_file):
        self.accounts_file = Path(accounts_file)

    def demo_login(self, level):
        accounts = json.loads(self.accounts_file.read_text(encoding='utf-8'))
        for badge, a in accounts.items():
            if a['level'] == int(level):
                return {'badge': badge, 'password': a['password'], 'code': totp_now(a['secret'])}
        return None


def die_with_us(child):
    """Windows: tie the child to a job object that kills it when this process ends for any reason (even a forced kill)."""
    if sys.platform != 'win32':
        return
    import ctypes
    from ctypes import wintypes

    class Basic(ctypes.Structure):
        _fields_ = [('a', ctypes.c_int64), ('b', ctypes.c_int64), ('LimitFlags', wintypes.DWORD), ('c', ctypes.c_size_t), ('d', ctypes.c_size_t),
                    ('e', wintypes.DWORD), ('f', ctypes.c_size_t), ('g', wintypes.DWORD), ('h', wintypes.DWORD)]

    class Extended(ctypes.Structure):
        _fields_ = [('Basic', Basic), ('io', ctypes.c_uint64 * 6), ('p', ctypes.c_size_t), ('q', ctypes.c_size_t), ('r', ctypes.c_size_t), ('s', ctypes.c_size_t)]

    k = ctypes.windll.kernel32
    k.CreateJobObjectW.restype = wintypes.HANDLE
    k.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    job = k.CreateJobObjectW(None, None)
    info = Extended()
    info.Basic.LimitFlags = 0x2000                      # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    k.SetInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info))
    k.AssignProcessToJobObject(job, wintypes.HANDLE(int(child._handle)))
    _JOBS.append(job)                                   # keep the handle open for the life of the process


_JOBS = []


def bundled_engine():
    """(cg executable, folder holding rules/ and data/) from the packaged app, or from a checkout's Rust build; None if absent."""
    bundle = Path(getattr(sys, '_MEIPASS', '')) / 'engine' if getattr(sys, '_MEIPASS', None) else None
    if bundle and (bundle / 'cg.exe').exists():
        return bundle / 'cg.exe', bundle
    exe = ROOT / 'rust' / 'target' / 'release' / ('cg.exe' if sys.platform == 'win32' else 'cg')
    return (exe, ROOT) if exe.exists() else None


def start_local_demo():
    """Run the server with demo data on a free local port. Returns (base url, bridge). The server is the Rust build (cg serve --demo),
    started as a child process that stops with the window; a source checkout without it falls back to the Python server."""
    port = free_port()
    data_dir = Path(tempfile.mkdtemp(prefix='claimguard-demo-'))
    found = bundled_engine()
    if found:
        exe, root = found
        sink = open(data_dir / 'server.log', 'w', encoding='utf-8')
        env = dict(os.environ, CLAIMGUARD_ROOT=str(root))
        flags = getattr(subprocess, 'CREATE_NO_WINDOW', 0)
        child = subprocess.Popen([str(exe), 'serve', '--demo', '--port', str(port), '--data-dir', str(data_dir)], stdout=sink, stderr=sink, env=env,
                                 creationflags=flags, cwd=str(root))
        atexit.register(child.kill)
        die_with_us(child)
    else:
        try:
            import serve_access
        except ImportError:
            raise SystemExit('the local demo needs the source checkout (it runs the real server); the installed app connects to a server instead') from None
        sink = open(data_dir / 'server.log', 'w', encoding='utf-8')
        threading.Thread(target=serve_access.main, args=(['--demo', '--port', str(port)],), kwargs={'out': sink, 'data_dir': data_dir}, daemon=True).start()
    base = f'http://127.0.0.1:{port}'
    if not wait_until_up(port):
        raise SystemExit('the local demo did not start; see ' + str(data_dir / 'server.log'))
    return base, DemoBridge(data_dir / 'demo_accounts.json')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--server', help='address of the ClaimGuard server (remembered)')
    parser.add_argument('--local-demo', action='store_true', help='start a private demo server on this PC')
    parser.add_argument('--browser', action='store_true', help='open the system browser instead of a window')
    args = parser.parse_args(argv)

    bridge = None
    if args.local_demo:
        base, bridge = start_local_demo()
    else:
        try:
            base = clean_server_url(args.server) if args.server else saved_server()
            if base is None:
                typed = ask_server()
                if not typed:
                    return 1
                base = clean_server_url(typed)
        except ValueError as e:
            print('error:', e)
            return 2
        remember_server(base)
    url = base + '/ui/'

    if args.browser:
        import webbrowser
        webbrowser.open(url)
        if args.local_demo:
            input('Demo running. Press Enter to stop.')
        return 0
    import webview
    webview.create_window(TITLE, url, width=1280, height=820, min_size=(900, 600), js_api=bridge)
    webview.start()
    return 0


if __name__ == '__main__':
    sys.exit(main())
