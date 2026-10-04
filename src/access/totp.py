"""Authenticator-app one-time codes (RFC 6238) and the encryption of each user's seed at rest."""
import hmac
import math

import pyotp
from cryptography.fernet import Fernet, InvalidToken


def new_secret():
    return pyotp.random_base32()


def provisioning_uri(secret, badge_id, issuer='ClaimGuard'):
    return pyotp.TOTP(secret).provisioning_uri(name=badge_id, issuer_name=issuer)


def encrypt_secret(secret, fernet_key):
    return Fernet(fernet_key.encode()).encrypt(secret.encode()).decode()


def decrypt_secret(token, fernet_key):
    try:
        return Fernet(fernet_key.encode()).decrypt(token.encode()).decode()
    except (InvalidToken, ValueError, TypeError, AttributeError, UnicodeError):
        raise ValueError('the stored secret cannot be decrypted') from None


def current_step(now, step=30):
    return int(math.floor(now / step))


def verify_code(secret, code, now, step=30):
    """The step number if `code` is the code for exactly the current step, otherwise None. The caller must then consume
    the step (UserStore.mark_totp_used) so the same code cannot be used twice."""
    if type(code) is not str or len(code) != 6 or not (code.isascii() and code.isdigit()):
        return None
    current = current_step(now, step)
    expected = pyotp.TOTP(secret, interval=step).at(current * step)
    return current if hmac.compare_digest(expected, code) else None
