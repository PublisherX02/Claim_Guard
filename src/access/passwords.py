"""Password hashing (bcrypt), a timing-equalising dummy check, and the password policy.

bcrypt only reads the first 72 bytes of a password, so longer ones are refused when set and never match when checked
(silently truncating would let two different long passwords be the same secret).
"""
import bcrypt

MIN_CHARS = 12
MAX_BYTES = 72
# Lower-case forms of passwords that satisfy the length and class rules and are still among the first things attackers try.
COMMON = frozenset({'password123!', 'welcome2024!', 'qwerty123456!', 'letmein12345!', 'admin1234567!', 'iloveyou12345',
                    'changeme1234!', 'p@ssw0rd1234'})
_dummy_hashes = {}


def hash_password(password, rounds):
    if not isinstance(password, str):
        raise ValueError('password must be text')
    raw = password.encode('utf-8')
    if len(raw) > MAX_BYTES:
        raise ValueError('password is longer than 72 bytes')
    return bcrypt.hashpw(raw, bcrypt.gensalt(rounds)).decode('ascii')


def verify_password(password, hashed):
    """True only for a matching password. Every kind of bad input is False, never an exception."""
    if not isinstance(password, str) or not isinstance(hashed, str) or not password or not hashed:
        return False
    try:
        raw = password.encode('utf-8')
        if len(raw) > MAX_BYTES:
            return False
        return bcrypt.checkpw(raw, hashed.encode('ascii'))
    except (ValueError, UnicodeError):
        return False


def dummy_verify(password, rounds):
    """Spend the time of one real check, so an unknown, locked or inactive account answers no faster than a real one."""
    hashed = _dummy_hashes.get(rounds)
    if hashed is None:
        hashed = _dummy_hashes[rounds] = bcrypt.hashpw(b'claimguard-dummy-password', bcrypt.gensalt(rounds))
    raw = password.encode('utf-8')[:MAX_BYTES] if isinstance(password, str) and password else b'x'
    try:
        bcrypt.checkpw(raw, hashed)
    except ValueError:
        pass


def check_policy(password, badge_id):
    """The reasons a password is not acceptable; an empty list means it is."""
    if not isinstance(password, str):
        return ['password must be text']
    problems = []
    if len(password) < MIN_CHARS:
        problems.append(f'password must be at least {MIN_CHARS} characters')
    if len(password.encode('utf-8')) > MAX_BYTES:
        problems.append(f'password must be at most {MAX_BYTES} bytes')
    digits = ''.join(ch for ch in str(badge_id) if ch.isdigit())
    if len(digits) >= 4 and digits in password:
        problems.append('password must not contain the badge number')
    if password.lower() in COMMON:
        problems.append('password is too common')
    classes = sum([any(c.islower() for c in password), any(c.isupper() for c in password),
                   any(c.isdigit() for c in password), any(not c.isalnum() for c in password)])
    if classes < 3:
        problems.append('password must use at least three character classes (lower case, upper case, digit, symbol)')
    return problems
