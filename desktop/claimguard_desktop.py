"""ClaimGuard for the desktop: the reviewer console in its own window.

    python desktop/claimguard_desktop.py --server https://claims.hospital.example      # a real server (remembered for next time)
    python desktop/claimguard_desktop.py --local-demo                                  # a private demo on this PC, nothing leaves it
    python desktop/claimguard_desktop.py --browser --server https://...                # no window toolkit: open the system browser

It is the same page the server serves to a browser (src/access/ui/), shown in a native window (Microsoft Edge WebView2 on Windows).
The window holds no data and no secret of its own: sign-in, the session cookie and every permission check are the server's. A remote
server must use https; plain http is accepted only for this PC (127.0.0.1, localhost). The local demo starts the real server on a free
port bound to this PC only, with generated demo accounts that exist for the life of the window.
"""
import argparse
import http.client
import json
import socket
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


class DemoBridge:
    """Exposed to the page only in the local demo (as window.pywebview.api): hands the sign-in form a generated demo login."""

    def __init__(self, accounts_file):
        self.accounts_file = Path(accounts_file)

    def demo_login(self, level):
        import pyotp
        accounts = json.loads(self.accounts_file.read_text(encoding='utf-8'))
        for badge, a in accounts.items():
            if a['level'] == int(level):
                return {'badge': badge, 'password': a['password'], 'code': pyotp.TOTP(a['secret']).now()}
        return None


def start_local_demo():
    """Run the real server with demo data on a free local port, in this process. Returns (base url, bridge)."""
    try:
        import serve_access
    except ImportError:
        raise SystemExit('the local demo needs the source checkout (it runs the real server); the installed app connects to a server instead') from None
    port = free_port()
    data_dir = Path(tempfile.mkdtemp(prefix='claimguard-demo-'))
    sink = open(data_dir / 'server.log', 'w', encoding='utf-8')
    threading.Thread(target=serve_access.main, args=(['--demo', '--port', str(port)],),
                     kwargs={'out': sink, 'data_dir': data_dir}, daemon=True).start()
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
