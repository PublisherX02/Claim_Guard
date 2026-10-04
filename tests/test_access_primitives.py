import base64
import json
import statistics
import sys
import time
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import jwt
import pyotp
from access import config, passwords, tokens, totp
from cryptography.fernet import Fernet

ROUNDS = 4


class PasswordTests(unittest.TestCase):
    def test_round_trip_and_wrong_password(self):
        h = passwords.hash_password('Tr1cky-Horse-Battery', ROUNDS)
        self.assertTrue(passwords.verify_password('Tr1cky-Horse-Battery', h))
        self.assertFalse(passwords.verify_password('Tr1cky-Horse-Batterz', h))

    def test_unicode_passwords_work(self):
        pw = 'pässwörd-Ünicode-123!'
        self.assertTrue(passwords.verify_password(pw, passwords.hash_password(pw, ROUNDS)))

    def test_hashes_are_salted(self):
        self.assertNotEqual(passwords.hash_password('same-Password-1', ROUNDS), passwords.hash_password('same-Password-1', ROUNDS))

    def test_a_73_byte_password_is_rejected_when_hashing_and_false_when_checking(self):
        long_pw = 'A1' * 37                                          # 74 bytes
        with self.assertRaises(ValueError):
            passwords.hash_password(long_pw, ROUNDS)
        self.assertFalse(passwords.verify_password(long_pw, passwords.hash_password('ok-Password-12', ROUNDS)))

    def test_garbage_inputs_are_false_not_exceptions(self):
        h = passwords.hash_password('ok-Password-12', ROUNDS)
        for pw in (None, 5, b'ok-Password-12', ['x'], ''):
            self.assertFalse(passwords.verify_password(pw, h), repr(pw))
        for bad in ('', 'x', None, 5, '$2b$04$short'):
            self.assertFalse(passwords.verify_password('ok-Password-12', bad), repr(bad))

    def test_dummy_verify_costs_about_as_much_as_a_real_check(self):
        rounds = 8
        h = passwords.hash_password('ok-Password-12', rounds)
        passwords.dummy_verify('warm-up', rounds)                  # the first call builds the cached hash
        real, dummy = [], []
        for _ in range(7):
            t = time.perf_counter(); passwords.verify_password('wrong-Password-1', h); real.append(time.perf_counter() - t)
            t = time.perf_counter(); passwords.dummy_verify('wrong-Password-1', rounds); dummy.append(time.perf_counter() - t)
        ratio = statistics.median(dummy) / statistics.median(real)
        self.assertTrue(0.5 < ratio < 2.0, ratio)

    def test_dummy_verify_never_raises_on_odd_input(self):
        for pw in (None, 5, 'x' * 500, ''):
            passwords.dummy_verify(pw, ROUNDS)


class PolicyTests(unittest.TestCase):
    def check(self, pw, badge='CG-0042'):
        return passwords.check_policy(pw, badge)

    def test_a_good_password_has_no_problems(self):
        self.assertEqual(self.check('Tr1cky-Horse-Battery'), [])

    def test_too_short_too_long_and_wrong_type(self):
        self.assertTrue(self.check('Ab1!xyz'))
        self.assertTrue(self.check('A1' * 37))
        self.assertTrue(self.check(None))

    def test_must_not_contain_the_badge_digits(self):
        self.assertTrue(any('badge' in p for p in self.check('Good-Pass-0042-xyz')))
        self.assertEqual(self.check('Good-Pass-0043-xyz'), [])

    def test_common_passwords_are_refused_whatever_the_case(self):
        for pw in ('Password123!', 'PASSWORD123!', 'password123!'):
            self.assertTrue(any('common' in p for p in self.check(pw)), pw)

    def test_needs_three_of_four_character_classes(self):
        self.assertTrue(any('classes' in p for p in self.check('alllowercaseonly')))
        self.assertTrue(any('classes' in p for p in self.check('lowercase12345678')))
        self.assertEqual(self.check('Lowercase12345678'), [])


class TotpTests(unittest.TestCase):
    NOW = 1_700_000_010

    def test_secret_is_base32_and_unique(self):
        a, b = totp.new_secret(), totp.new_secret()
        self.assertEqual(len(a), 32)
        self.assertTrue(set(a) <= set('ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'))
        self.assertNotEqual(a, b)

    def test_provisioning_uri_carries_the_secret_issuer_and_badge(self):
        s = totp.new_secret()
        uri = totp.provisioning_uri(s, 'CG-0042')
        parts = urlparse(uri)
        self.assertEqual((parts.scheme, parts.netloc), ('otpauth', 'totp'))
        q = parse_qs(parts.query)
        self.assertEqual(q['secret'], [s])
        self.assertEqual(q['issuer'], ['ClaimGuard'])
        self.assertIn('CG-0042', uri)

    def test_secrets_are_encrypted_at_rest_and_tamper_evident(self):
        key = Fernet.generate_key().decode()
        s = totp.new_secret()
        enc = totp.encrypt_secret(s, key)
        self.assertNotIn(s, enc)
        self.assertEqual(totp.decrypt_secret(enc, key), s)
        flipped = enc[:-4] + ('A' if enc[-4] != 'A' else 'B') + enc[-3:]
        with self.assertRaises(ValueError):
            totp.decrypt_secret(flipped, key)
        with self.assertRaises(ValueError):
            totp.decrypt_secret(enc, Fernet.generate_key().decode())
        with self.assertRaises(ValueError):
            totp.decrypt_secret('not a token', key)

    def test_only_the_code_for_the_current_step_is_accepted(self):
        s = totp.new_secret()
        gen = pyotp.TOTP(s)
        step = totp.current_step(self.NOW)
        self.assertEqual(totp.verify_code(s, gen.at(self.NOW), self.NOW), step)
        self.assertIsNone(totp.verify_code(s, gen.at(self.NOW - 30), self.NOW))
        self.assertIsNone(totp.verify_code(s, gen.at(self.NOW + 30), self.NOW))

    def test_malformed_codes_are_none(self):
        s = totp.new_secret()
        good = pyotp.TOTP(s).at(self.NOW)
        for bad in ('12345', '1234567', 'abcdef', ' ' + good, good + ' ', int(good), None, b'123456', ['123456'], '１２３４５６'):
            self.assertIsNone(totp.verify_code(s, bad, self.NOW), repr(bad))

    def test_step_boundaries_follow_floor_division(self):
        self.assertEqual(totp.current_step(30 * 1000), 1000)
        self.assertEqual(totp.current_step(30 * 1000 - 0.001), 999)
        self.assertEqual(totp.current_step(30 * 1000 + 29.999), 1000)


class TokenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.settings = config.load_settings({}, dev=True)
        cls.now = 1_700_000_000

    def issue(self, now=None):
        return tokens.issue(self.settings, 'CG-0042', now=now or self.now)

    def forge(self, secret=None, alg='HS256', **claims):
        base = {'sub': 'CG-0042', 'jti': 'j1', 'csrf': 'c1', 'iat': self.now, 'exp': self.now + 600}
        base = {k: v for k, v in {**base, **claims}.items() if v is not None}
        return jwt.encode(base, secret or self.settings.jwt_secret, algorithm=alg)

    def test_round_trip(self):
        i = self.issue()
        d = tokens.decode(self.settings, i.token, now=self.now + 5)
        self.assertEqual((d['sub'], d['jti'], d['csrf']), ('CG-0042', i.jti, i.csrf))
        self.assertEqual(d['exp'] - d['iat'], self.settings.token_ttl_seconds)
        self.assertEqual(i.expires_at, d['exp'])

    def test_expiry_is_exact_with_no_leeway(self):
        i = self.issue()
        tokens.decode(self.settings, i.token, now=i.expires_at - 1)
        for now in (i.expires_at, i.expires_at + 1):
            with self.assertRaises(tokens.TokenError):
                tokens.decode(self.settings, i.token, now=now)

    def test_a_token_from_the_future_is_refused(self):
        i = self.issue()
        with self.assertRaises(tokens.TokenError):
            tokens.decode(self.settings, i.token, now=self.now - 10)

    def test_wrong_secret_wrong_algorithm_and_none_are_refused(self):
        other = config.load_settings({}, dev=True)
        with self.assertRaises(tokens.TokenError):
            tokens.decode(other, self.issue().token, now=self.now)
        with self.assertRaises(tokens.TokenError):
            tokens.decode(self.settings, self.forge(alg='HS384'), now=self.now)
        with self.assertRaises(tokens.TokenError):
            tokens.decode(self.settings, self.forge(alg='HS512'), now=self.now)

        def b64(o):
            return base64.urlsafe_b64encode(json.dumps(o).encode()).rstrip(b'=').decode()
        none_token = b64({'alg': 'none', 'typ': 'JWT'}) + '.' + b64({'sub': 'CG-0001', 'jti': 'j', 'csrf': 'c', 'iat': self.now, 'exp': self.now + 600}) + '.'
        with self.assertRaises(tokens.TokenError):
            tokens.decode(self.settings, none_token, now=self.now)

    def test_each_required_claim_is_required(self):
        tokens.decode(self.settings, self.forge(), now=self.now + 1)           # the forge helper itself is valid
        for missing in ('sub', 'jti', 'csrf', 'iat', 'exp'):
            tok = jwt.encode({k: v for k, v in {'sub': 'CG-0042', 'jti': 'j1', 'csrf': 'c1', 'iat': self.now, 'exp': self.now + 600}.items()
                              if k != missing}, self.settings.jwt_secret, algorithm='HS256')
            with self.assertRaises(tokens.TokenError, msg=missing):
                tokens.decode(self.settings, tok, now=self.now + 1)

    def test_claims_must_have_the_right_types(self):
        for claims in ({'sub': 5}, {'jti': ['x']}, {'csrf': 7}, {'exp': 'soon'}):
            with self.assertRaises(tokens.TokenError, msg=str(claims)):
                tokens.decode(self.settings, self.forge(**claims), now=self.now + 1)

    def test_tampering_with_any_segment_is_refused(self):
        head, payload, sig = self.issue().token.split('.')
        for tampered in (head + '.' + payload[:-2] + ('AA' if payload[-2:] != 'AA' else 'BB') + '.' + sig,
                         head + '.' + payload + '.' + sig[:-2] + ('AA' if sig[-2:] != 'AA' else 'BB'),
                         payload + '.' + head + '.' + sig):
            with self.assertRaises(tokens.TokenError):
                tokens.decode(self.settings, tampered, now=self.now + 1)

    def test_garbage_is_a_token_error_and_never_another_exception(self):
        for bad in (None, 5, b'abc', [], '', 'a.b.c', '.' * 5, 'x' * 3_000_000, '\x00\x00', 'ey' * 100):
            with self.assertRaises(tokens.TokenError, msg=repr(bad)[:40]):
                tokens.decode(self.settings, bad, now=self.now)

    def test_ids_are_unique_across_a_thousand_tokens(self):
        issued = [self.issue() for _ in range(1000)]
        self.assertEqual(len({i.jti for i in issued}), 1000)
        self.assertEqual(len({i.csrf for i in issued}), 1000)
        self.assertNotEqual(issued[0].jti, issued[0].csrf)


if __name__ == '__main__':
    unittest.main()
