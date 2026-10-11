"""RFC 3161 trusted timestamp for the audit log's anchor (docs/20 finding F4, docs/32).

The audit log's anchor file (<log>.head.json) holds the chain head and the event count. An HMAC protects it only while the
key stays secret. A timestamp token from an independent Time-Stamp Authority (TSA) proves the head hash existed at a given
time and is signed by a party the log writer does not control, so a later rewrite of the chain cannot also rewrite the
token.

Design choices (each is tested in tests/test_timestamp_anchor.py):
- What is stamped: SHA-256 of the ASCII string "<head>|<count>" (the same string the HMAC covers), not every event. One token
  covers the whole chain up to that point because every row hashes its predecessor.
- Where it is stored: a sidecar file <log>.head.tsr (JSON: the base64 DER token, the stamped head and count, the time it was
  requested). The anchor file and the log format do not change, so logs and anchors written before this feature still verify.
- It never blocks anything: stamping is a separate step (scripts/timestamp_log.py). AuditLog appends never call a network.
  A TSA outage leaves the previous token in place and the verifier reports the log as "untimestamped since <count>".
- What verification checks: PKIStatus is granted; the message imprint equals the digest of the stamped head and count; the
  CMS signature over the TSTInfo is valid for the signer certificate; that certificate carries the timeStamping extended key
  usage, was valid at genTime, and was issued directly by a CA the verifier trusts (a PEM file the verifier supplies; the
  certificates inside the token are never trusted on their own); the token's nonce matches the request.

Dependencies: pyasn1 and pyasn1-modules (ASN.1 structures of RFC 3161 and CMS) and cryptography (signature and certificate
checks). The network call uses urllib from the standard library.
"""
import base64
import hashlib
import json
import os
import secrets
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, padding, rsa
from cryptography.x509.oid import ExtendedKeyUsageOID
from pyasn1.codec.der import decoder, encoder
from pyasn1.type import univ
from pyasn1_modules import rfc2459, rfc3161, rfc5652

OID_SHA256 = univ.ObjectIdentifier('2.16.840.1.101.3.4.2.1')
OID_MESSAGE_DIGEST = univ.ObjectIdentifier('1.2.840.113549.1.9.4')
OID_TST_INFO = univ.ObjectIdentifier('1.2.840.113549.1.9.16.1.4')
OID_SIGNED_DATA = univ.ObjectIdentifier('1.2.840.113549.1.7.2')
MAX_RESPONSE_BYTES = 1_000_000
SIDECAR_SUFFIX = '.head.tsr'


class TimestampError(Exception):
    """The token is missing, malformed, or does not prove what it claims."""


def stamped_message(head, count):
    return f'{head}|{count}'.encode('ascii')


def imprint(head, count):
    return hashlib.sha256(stamped_message(head, count)).digest()


def build_request(digest, nonce):
    """DER TimeStampReq for a SHA-256 digest, asking for the TSA certificate to be included."""
    req = rfc3161.TimeStampReq()
    req['version'] = 1
    mi = req['messageImprint']
    mi['hashAlgorithm']['algorithm'] = OID_SHA256
    mi['hashAlgorithm']['parameters'] = univ.Null('')
    mi['hashedMessage'] = digest
    req['nonce'] = nonce
    req['certReq'] = True
    return encoder.encode(req)


def request_token(tsa_url, digest, nonce, timeout=10, opener=None):
    """POST the request to the TSA and return the DER response. Raises TimestampError on any failure; callers must treat
    that as 'untimestamped', never as a reason to stop writing the log."""
    body = build_request(digest, nonce)
    req = urllib.request.Request(tsa_url, data=body, headers={'Content-Type': 'application/timestamp-query'})
    try:
        with (opener or urllib.request.urlopen)(req, timeout=timeout) as resp:
            data = resp.read(MAX_RESPONSE_BYTES + 1)
    except Exception as exc:  # network, DNS, HTTP, TLS: all mean the same thing here
        raise TimestampError(f'TSA request failed: {exc}') from exc
    if len(data) > MAX_RESPONSE_BYTES:
        raise TimestampError('TSA response is larger than 1 MB')
    return data


def _hash_for(oid):
    # Only SHA-256 is accepted for the signature digest: an unknown or weak algorithm fails closed.
    if oid == OID_SHA256:
        return hashes.SHA256()
    raise TimestampError(f'unsupported digest algorithm {oid}')


def _signed_attrs_der(attrs):
    # RFC 5652 section 5.4: the signature covers the DER of signedAttrs with the universal SET OF tag (0x31), while the
    # structure carries the implicit [0] tag (0xA0).
    der = bytearray(encoder.encode(attrs))
    if der[0] != 0xA0:
        raise TimestampError('signedAttrs has an unexpected tag')
    der[0] = 0x31
    return bytes(der)


def _check_signature(cert, signature, data, sig_alg_oid, digest_alg):
    pub = cert.public_key()
    try:
        if isinstance(pub, rsa.RSAPublicKey):
            pub.verify(signature, data, padding.PKCS1v15(), digest_alg)
        elif isinstance(pub, ec.EllipticCurvePublicKey):
            pub.verify(signature, data, ec.ECDSA(digest_alg))
        else:
            raise TimestampError('unsupported signer key type')
    except InvalidSignature as exc:
        raise TimestampError('the token signature does not verify') from exc


def verify_token(response_der, head, count, trusted_ca_pem, expected_nonce=None):
    """Verify a DER TimeStampResp (or bare token) for the given head and count. Returns {'gen_time', 'tsa_subject', 'serial'}.
    Raises TimestampError on any problem. trusted_ca_pem is the PEM text (one or more certificates) of the CAs allowed to
    issue the TSA certificate."""
    try:
        resp, rest = decoder.decode(response_der, asn1Spec=rfc3161.TimeStampResp())
    except Exception as exc:
        raise TimestampError(f'not a TimeStampResp: {exc}') from exc
    if rest:
        raise TimestampError('trailing bytes after the response')
    status = int(resp['status']['status'])
    if status not in (0, 1):  # granted, grantedWithMods
        raise TimestampError(f'TSA refused the request (PKIStatus {status})')
    token = resp['timeStampToken']
    if not token.isValue or token['contentType'] != OID_SIGNED_DATA:
        raise TimestampError('response carries no signed-data token')
    try:
        signed, _ = decoder.decode(bytes(token['content']), asn1Spec=rfc5652.SignedData())
    except Exception as exc:
        raise TimestampError(f'token is not CMS SignedData: {exc}') from exc
    encap = signed['encapContentInfo']
    if encap['eContentType'] != OID_TST_INFO or not encap['eContent'].isValue:
        raise TimestampError('token does not contain TSTInfo')
    tst_der = bytes(encap['eContent'])
    tst, _ = decoder.decode(tst_der, asn1Spec=rfc3161.TSTInfo())

    mi = tst['messageImprint']
    if mi['hashAlgorithm']['algorithm'] != OID_SHA256 or bytes(mi['hashedMessage']) != imprint(head, count):
        raise TimestampError('the token stamps a different head or count than the one claimed')
    if expected_nonce is not None and (not tst['nonce'].isValue or int(tst['nonce']) != expected_nonce):
        raise TimestampError('the token nonce does not match the request')
    gen_time = datetime.strptime(str(tst['genTime']).split('.')[0].rstrip('Z'), '%Y%m%d%H%M%S').replace(tzinfo=timezone.utc)

    if len(signed['signerInfos']) != 1:
        raise TimestampError('expected exactly one signer')
    si = signed['signerInfos'][0]
    if not si['signedAttrs'].isValue:
        raise TimestampError('signer has no signed attributes')
    digest_alg = _hash_for(si['digestAlgorithm']['algorithm'])
    h = hashes.Hash(digest_alg)
    h.update(tst_der)
    tst_digest = h.finalize()
    attr_digest = None
    for attr in si['signedAttrs']:
        if attr['attrType'] == OID_MESSAGE_DIGEST:
            vals, _ = decoder.decode(bytes(attr['attrValues'][0]), asn1Spec=univ.OctetString())
            attr_digest = bytes(vals)
    if attr_digest != tst_digest:
        raise TimestampError('messageDigest attribute does not match the TSTInfo')

    ca_certs = x509.load_pem_x509_certificates(trusted_ca_pem if isinstance(trusted_ca_pem, bytes)
                                               else trusted_ca_pem.encode('ascii'))
    if not ca_certs:
        raise TimestampError('no trusted CA certificate supplied')
    candidates = []
    for c in signed['certificates'] if signed['certificates'].isValue else []:
        if c['certificate'].isValue:
            candidates.append(x509.load_der_x509_certificate(encoder.encode(c['certificate'])))
    signer_cert = None
    sid = si['sid']['issuerAndSerialNumber']
    want_serial = int(sid['serialNumber'])
    for c in candidates:
        if c.serial_number == want_serial:
            signer_cert = c
    if signer_cert is None:
        raise TimestampError('the signer certificate is not in the token')

    _check_signature(signer_cert, bytes(si['signature']), _signed_attrs_der(si['signedAttrs']),
                     si['signatureAlgorithm']['algorithm'], digest_alg)

    if not any(_issued_by(signer_cert, ca) for ca in ca_certs):
        raise TimestampError('the signer certificate was not issued by a trusted CA')
    if not (signer_cert.not_valid_before_utc <= gen_time <= signer_cert.not_valid_after_utc):
        raise TimestampError('the signer certificate was not valid at the stamped time')
    try:
        eku = signer_cert.extensions.get_extension_for_class(x509.ExtendedKeyUsage)
    except x509.ExtensionNotFound as exc:
        raise TimestampError('the signer certificate has no extended key usage') from exc
    if ExtendedKeyUsageOID.TIME_STAMPING not in eku.value or not eku.critical:
        raise TimestampError('the signer certificate is not a critical timeStamping certificate')
    return {'gen_time': gen_time.isoformat(), 'tsa_subject': signer_cert.subject.rfc4514_string(),
            'serial': str(int(tst['serialNumber']))}


def _issued_by(cert, ca):
    try:
        cert.verify_directly_issued_by(ca)
        return True
    except Exception:
        return False


def sidecar_path(log_path):
    p = Path(log_path)
    return p.with_name(p.name + SIDECAR_SUFFIX)


def stamp_log(log_path, anchor_path, tsa_url, timeout=10, opener=None):
    """Request a token for the log's current anchor and store it. Returns the sidecar dict. Raises TimestampError on any
    failure and leaves the previous sidecar untouched."""
    anchor = json.loads(Path(anchor_path).read_text(encoding='utf-8'))
    head, count = anchor['head'], anchor['count']
    nonce = secrets.randbits(63)
    der = request_token(tsa_url, imprint(head, count), nonce, timeout=timeout, opener=opener)
    side = {'head': head, 'count': count, 'nonce': nonce, 'tsa_url': tsa_url,
            'requested_at': datetime.now(timezone.utc).isoformat(), 'token': base64.b64encode(der).decode('ascii')}
    tmp = sidecar_path(log_path).with_name(sidecar_path(log_path).name + f'.{os.getpid()}.tmp')
    tmp.write_text(json.dumps(side, indent=2), encoding='utf-8')
    os.replace(tmp, sidecar_path(log_path))
    return side


def timestamp_status(log_path, anchor_path, trusted_ca_pem=None):
    """Report the timestamp state separately from the chain state.

    state is one of:
      'none'        no sidecar: the log was never timestamped
      'current'     a valid token covers the anchor's present head and count
      'stale'       a valid token covers an EARLIER head and count of this same log; events after it are not yet stamped
      'unverified'  a sidecar exists but no trusted CA was supplied, so the token was NOT checked
      'invalid'     the token fails verification, or stamps a head that is not part of this log
    """
    sp = sidecar_path(log_path)
    if not sp.exists():
        return {'state': 'none'}
    side = json.loads(sp.read_text(encoding='utf-8'))
    if not trusted_ca_pem:
        return {'state': 'unverified', 'stamped_count': side['count']}
    try:
        info = verify_token(base64.b64decode(side['token']), side['head'], side['count'], trusted_ca_pem,
                            expected_nonce=side.get('nonce'))
    except (TimestampError, ValueError, KeyError) as exc:
        return {'state': 'invalid', 'reason': str(exc)}
    anchor = json.loads(Path(anchor_path).read_text(encoding='utf-8'))
    info['stamped_count'] = side['count']
    if side['head'] == anchor['head'] and side['count'] == anchor['count']:
        info['state'] = 'current'
        return info
    if side['count'] < anchor['count'] and _head_in_log(log_path, side['head'], side['count']):
        info['state'] = 'stale'
        info['unstamped_events'] = anchor['count'] - side['count']
        return info
    return {'state': 'invalid', 'reason': 'the stamped head is not a point in this log (the log was rewritten or replaced)'}


def _head_in_log(log_path, head, count):
    """True when the row at position count has hash head: the stamped state is a prefix of the present chain."""
    with open(log_path, encoding='utf-8') as f:
        for n, line in enumerate(f, 1):
            if n == count:
                return json.loads(line)['hash'] == head
    return False
