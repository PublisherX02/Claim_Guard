"""A throwaway RFC 3161 Time-Stamp Authority for tests: generates its own CA and TSA certificate and signs real responses.
No network, no shared secret. Used by tests/test_timestamp_anchor.py."""
import hashlib
from datetime import datetime, timedelta, timezone

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
from pyasn1.codec.der import decoder, encoder
from pyasn1.type import univ, useful
from pyasn1_modules import rfc3161, rfc5652

import timestamp_anchor as ta


def _name(cn):
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])


def make_ca(cn='Test Root CA'):
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    now = datetime.now(timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(_name(cn)).issuer_name(_name(cn)).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - timedelta(days=1))
            .not_valid_after(now + timedelta(days=30))
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .sign(key, hashes.SHA256()))
    return key, cert


def make_tsa_cert(ca_key, ca_cert, cn='Test TSA', eku_critical=True, add_eku=True, not_before=None, not_after=None):
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    now = datetime.now(timezone.utc)
    b = (x509.CertificateBuilder().subject_name(_name(cn)).issuer_name(ca_cert.subject).public_key(key.public_key())
         .serial_number(x509.random_serial_number()).not_valid_before(not_before or now - timedelta(days=1))
         .not_valid_after(not_after or now + timedelta(days=10)))
    if add_eku:
        b = b.add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.TIME_STAMPING]), critical=eku_critical)
    return key, b.sign(ca_key, hashes.SHA256())


def pem(*certs):
    return b''.join(c.public_bytes(serialization.Encoding.PEM) for c in certs)


class LocalTSA:
    def __init__(self, ca_key=None, ca_cert=None, tsa_key=None, tsa_cert=None, gen_time=None, serial=1):
        self.ca_key, self.ca_cert = (ca_key, ca_cert) if ca_cert else make_ca()
        self.tsa_key, self.tsa_cert = (tsa_key, tsa_cert) if tsa_cert else make_tsa_cert(self.ca_key, self.ca_cert)
        self.gen_time = gen_time
        self.serial = serial
        self.requests = []
        self.fail_with = None

    @property
    def trusted_pem(self):
        return pem(self.ca_cert)

    def respond(self, request_der, tamper_imprint=False, tamper_signature=False, status=0, nonce_override=None):
        req, _ = decoder.decode(request_der, asn1Spec=rfc3161.TimeStampReq())
        self.requests.append(req)
        resp = rfc3161.TimeStampResp()
        resp['status']['status'] = status
        if status not in (0, 1):
            return encoder.encode(resp)
        digest = bytes(req['messageImprint']['hashedMessage'])
        if tamper_imprint:
            digest = hashlib.sha256(b'something else').digest()

        tst = rfc3161.TSTInfo()
        tst['version'] = 1
        tst['policy'] = univ.ObjectIdentifier('1.2.3.4.1')
        tst['messageImprint']['hashAlgorithm']['algorithm'] = ta.OID_SHA256
        tst['messageImprint']['hashAlgorithm']['parameters'] = univ.Null('')
        tst['messageImprint']['hashedMessage'] = digest
        tst['serialNumber'] = self.serial
        when = self.gen_time or datetime.now(timezone.utc)
        tst['genTime'] = useful.GeneralizedTime(when.strftime('%Y%m%d%H%M%SZ'))
        tst['nonce'] = nonce_override if nonce_override is not None else int(req['nonce'])
        tst_der = encoder.encode(tst)

        signed = rfc5652.SignedData()
        signed['version'] = 3
        da = signed['digestAlgorithms']
        da.append(rfc5652.DigestAlgorithmIdentifier())
        da[0]['algorithm'] = ta.OID_SHA256
        signed['encapContentInfo']['eContentType'] = ta.OID_TST_INFO
        signed['encapContentInfo']['eContent'] = tst_der
        cert_asn1, _ = decoder.decode(self.tsa_cert.public_bytes(serialization.Encoding.DER),
                                      asn1Spec=rfc5652.CertificateChoices()['certificate'])
        signed['certificates'][0]['certificate'] = cert_asn1

        si = rfc5652.SignerInfo()
        si['version'] = 1
        si['sid']['issuerAndSerialNumber']['issuer'] = cert_asn1['tbsCertificate']['issuer']
        si['sid']['issuerAndSerialNumber']['serialNumber'] = cert_asn1['tbsCertificate']['serialNumber']
        si['digestAlgorithm']['algorithm'] = ta.OID_SHA256
        si['digestAlgorithm']['parameters'] = univ.Null('')
        attrs = si['signedAttrs']
        a0 = rfc5652.Attribute()
        a0['attrType'] = univ.ObjectIdentifier('1.2.840.113549.1.9.3')  # contentType
        a0['attrValues'].append(encoder.encode(ta.OID_TST_INFO))
        a1 = rfc5652.Attribute()
        a1['attrType'] = ta.OID_MESSAGE_DIGEST
        a1['attrValues'].append(encoder.encode(univ.OctetString(hashlib.sha256(tst_der).digest())))
        attrs.append(a0)
        attrs.append(a1)
        si['signatureAlgorithm']['algorithm'] = univ.ObjectIdentifier('1.2.840.113549.1.1.1')
        si['signatureAlgorithm']['parameters'] = univ.Null('')
        sig = self.tsa_key.sign(ta._signed_attrs_der(attrs), padding.PKCS1v15(), hashes.SHA256())
        if tamper_signature:
            sig = bytes([sig[0] ^ 1]) + sig[1:]
        si['signature'] = sig
        signed['signerInfos'].append(si)

        resp['timeStampToken']['contentType'] = ta.OID_SIGNED_DATA
        resp['timeStampToken']['content'] = encoder.encode(signed)
        return encoder.encode(resp)

    def opener(self, **kw):
        """A urlopen replacement that answers from this TSA, or raises when fail_with is set."""
        outer = self

        class _Resp:
            def __init__(self, data):
                self._d = data

            def read(self, n=-1):
                return self._d

            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        def _open(req, timeout=None):
            if outer.fail_with:
                raise outer.fail_with
            return _Resp(outer.respond(req.data, **kw))
        return _open
