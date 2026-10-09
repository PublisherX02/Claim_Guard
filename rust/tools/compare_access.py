"""Cross-check of the access primitives: what one build writes the other must read.

    python rust/tools/compare_access.py [--cg rust/target/release/cg.exe]

Password hashes (bcrypt), signed session tokens (HS256), encrypted authenticator seeds (Fernet), one-time codes (RFC 6238), provisioning
URIs and masking pseudonyms, in both directions. Exit code 0 only when everything agrees.
"""
import argparse, json, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
from access import masking, passwords, tokens, totp  # noqa: E402
from access.config import load_settings  # noqa: E402
from cryptography.fernet import Fernet  # noqa: E402


class Rust:
    def __init__(self, cg):
        self.p = subprocess.Popen([cg, 'xcheck'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding='utf-8', bufsize=1)

    def ask(self, **req):
        self.p.stdin.write(json.dumps(req) + '\n')
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=10)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    a = ap.parse_args()
    rs = Rust(a.cg)
    failures = []

    def check(name, ok, detail=''):
        print(('ok   ' if ok else 'FAIL ') + name + (f'  {detail}' if detail and not ok else ''))
        if not ok:
            failures.append(name)

    # passwords
    pws = ['Correct-Horse-9', 'pässwörd-Läng-99', 'A1!' * 24, 'with space and é \U0001f600 1A']
    for pw in pws:
        h_py = passwords.hash_password(pw, 4)
        check(f'Rust verifies a Python bcrypt hash of {pw[:14]!r}', rs.ask(op='verify_password', password=pw, hash=h_py) is True)
        check(f'Rust rejects a wrong password against it', rs.ask(op='verify_password', password=pw + 'x', hash=h_py) is False)
        h_rs = rs.ask(op='hash_password', password=pw, rounds=4)
        check(f'Python verifies a Rust bcrypt hash of {pw[:14]!r}', isinstance(h_rs, str) and passwords.verify_password(pw, h_rs))
    check('both refuse a 73-byte password', rs.ask(op='hash_password', password='a' * 73, rounds=4) is None and not passwords.verify_password('a' * 73, passwords.hash_password('a' * 72, 4)))

    # tokens
    secret = 'S' * 48
    st = load_settings({'JWT_SECRET': secret, 'BCRYPT_ROUNDS': '4'}, dev=True)
    now = 1_700_000_000
    t_py = tokens.issue(st, 'CG-2002', now=now)
    got = rs.ask(op='decode_token', token=t_py.token, secret=secret, now=now + 5)
    check('Rust decodes a Python session token', got and got['sub'] == 'CG-2002' and got['jti'] == t_py.jti and got['csrf'] == t_py.csrf)
    check('Rust refuses it after it expired', rs.ask(op='decode_token', token=t_py.token, secret=secret, now=t_py.expires_at) is None)
    check('Rust refuses it with another secret', rs.ask(op='decode_token', token=t_py.token, secret='T' * 48, now=now + 5) is None)
    t_rs = rs.ask(op='issue_token', secret=secret, badge='CG-3003', now=now)
    try:
        p = tokens.decode(st, t_rs['token'], now=now + 5)
        check('Python decodes a Rust session token', p['sub'] == 'CG-3003' and p['jti'] == t_rs['jti'] and p['exp'] == t_rs['expires_at'])
    except tokens.TokenError:
        check('Python decodes a Rust session token', False)
    try:
        tokens.decode(st, t_rs['token'], now=t_rs['expires_at'])
        check('Python refuses a Rust token after it expired', False)
    except tokens.TokenError:
        check('Python refuses a Rust token after it expired', True)

    # encrypted seeds
    key = Fernet.generate_key().decode()
    seed = totp.new_secret()
    check('Rust decrypts a seed encrypted by Python', rs.ask(op='fernet_decrypt', token=totp.encrypt_secret(seed, key), key=key) == seed)
    enc = rs.ask(op='fernet_encrypt', plain=seed, key=key)
    check('Python decrypts a seed encrypted by Rust', isinstance(enc, str) and totp.decrypt_secret(enc, key) == seed)
    check('Rust refuses a seed under another key', rs.ask(op='fernet_decrypt', token=totp.encrypt_secret(seed, key), key=Fernet.generate_key().decode()) is None)

    # one-time codes and the authenticator URI
    import pyotp
    mismatches = 0
    for i in range(200):
        s = totp.new_secret()
        step = 56_000_000 + i * 7919
        if rs.ask(op='totp_code', secret=s, step=step) != pyotp.TOTP(s).at(step * 30):
            mismatches += 1
    check('200 one-time codes agree with pyotp', mismatches == 0, f'{mismatches} differ')
    for badge in ('CG-2002', 'CG-1234 x&y'):
        check(f'provisioning URI for {badge!r} agrees', rs.ask(op='provisioning_uri', secret=seed, badge=badge) == totp.provisioning_uri(seed, badge))

    # pseudonyms
    mk = 'k' * 40
    ok = True
    for prefix, value in (('PAT', 'PAT-7'), ('MEM', 'MEM-éè'), ('PAT', 'x' * 200), ('MEM', 'a b')):
        ok &= rs.ask(op='pseudonym', key=mk, prefix=prefix, value=value) == masking.pseudonym(mk, prefix, value)
    check('masking pseudonyms agree', ok)

    rs.close()
    print('ALL AGREE' if not failures else f'{len(failures)} checks failed')
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
