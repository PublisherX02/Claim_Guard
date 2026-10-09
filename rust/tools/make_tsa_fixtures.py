"""Writes the RFC 3161 fixtures the Rust verifier is tested against, signed by the Python test authority (tests/local_tsa.py).

    python rust/tools/make_tsa_fixtures.py

Each case in manifest.json names a response file, the head and count claimed, the nonce, the CA file to trust and what the Python verifier
(src/timestamp_anchor.py) says about it: "ok", or the start of its error text. The Rust tests read the same manifest and must agree on
every case. Fixtures stay valid forever because certificates are checked against the stamped time, not the clock.
"""
import json, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import timestamp_anchor as ta  # noqa: E402
from local_tsa import LocalTSA, make_ca, make_tsa_cert, pem  # noqa: E402

OUT = ROOT / 'rust' / 'crates' / 'cg-audit' / 'tests' / 'fixtures'
HEAD, COUNT, NONCE = 'ab' * 32, 7, 12345


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    good = LocalTSA()
    other = LocalTSA()                                     # a second, unrelated authority
    req = ta.build_request(ta.imprint(HEAD, COUNT), NONCE)
    (OUT / 'request.der').write_bytes(req)
    (OUT / 'ca.pem').write_bytes(good.trusted_pem)
    (OUT / 'other_ca.pem').write_bytes(other.trusted_pem)
    (OUT / 'two_cas.pem').write_bytes(good.trusted_pem + other.trusted_pem)
    cases = []

    def add(name, der, head=HEAD, count=COUNT, nonce=NONCE, ca='ca.pem', desc=''):
        (OUT / f'{name}.tsr').write_bytes(der)
        try:
            info = ta.verify_token(der, head, count, (OUT / ca).read_bytes(), expected_nonce=nonce)
            expect = 'ok'
        except ta.TimestampError as e:
            info, expect = None, str(e)
        cases.append({'name': name, 'file': f'{name}.tsr', 'head': head, 'count': count, 'nonce': nonce, 'ca': ca, 'python_says': expect,
                      'info': info, 'desc': desc})

    add('good', good.respond(req), desc='a valid token')
    add('good_two_cas', good.respond(req), ca='two_cas.pem', desc='the issuer is one of two trusted CAs')
    add('other_head', good.respond(req), head='cd' * 32, desc='claimed head differs from the stamped one')
    add('other_count', good.respond(req), count=8, desc='claimed count differs')
    add('wrong_nonce_claimed', good.respond(req), nonce=999, desc='the caller expects another nonce')
    add('nonce_override', good.respond(req, nonce_override=999), desc='the TSA answered with another nonce')
    add('tampered_imprint', good.respond(req, tamper_imprint=True), desc='the TSA stamped some other digest')
    add('tampered_signature', good.respond(req, tamper_signature=True), desc='a flipped signature byte')
    add('untrusted_issuer', good.respond(req), ca='other_ca.pem', desc='the signer was not issued by the trusted CA')
    add('refused', good.respond(req, status=2), desc='PKIStatus rejection')
    add('garbage', b'not a timestamp response', desc='garbage bytes')
    add('trailing_bytes', good.respond(req) + b'\x00', desc='bytes after the response')
    now = datetime.now(timezone.utc)
    key, cert = make_tsa_cert(good.ca_key, good.ca_cert, add_eku=False)
    add('no_eku', LocalTSA(good.ca_key, good.ca_cert, key, cert).respond(req), desc='signer certificate has no extended key usage')
    key, cert = make_tsa_cert(good.ca_key, good.ca_cert, eku_critical=False)
    add('eku_not_critical', LocalTSA(good.ca_key, good.ca_cert, key, cert).respond(req), desc='timeStamping usage not marked critical')
    add('not_valid_at_stamp_time', LocalTSA(good.ca_key, good.ca_cert, good.tsa_key, good.tsa_cert, gen_time=now + timedelta(days=400)).respond(req),
        desc='stamped after the signer certificate expired')
    # the bit-flip family: change one byte of the signed content and see that nothing still verifies
    base = good.respond(req)
    for i, pos in enumerate((len(base) // 4, len(base) // 2, (3 * len(base)) // 4, len(base) - 5)):
        flipped = bytearray(base)
        flipped[pos] ^= 0x01
        add(f'bitflip_{i}', bytes(flipped), desc=f'byte {pos} of {len(base)} flipped')
    (OUT / 'manifest.json').write_text(json.dumps(cases, indent=1), encoding='utf-8')
    print(len(cases), 'cases written to', OUT)
    for c in cases:
        print(f"  {c['name']:26} python: {c['python_says'][:70]}")


if __name__ == '__main__':
    main()
