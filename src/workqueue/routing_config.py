"""The routing configuration: every number that decides where a claim goes and how much work an agent holds.

Nothing here is a constant of the code paths. An administrator changes these values (through the queue service, which writes a
new numbered version and records before and after in the security log), each triage receipt names the version it used, and an
invalid configuration is refused whole, never applied in part.
"""
from dataclasses import dataclass, field, fields

STATUSES = ('FAIL', 'UNABLE_TO_ASSESS')
SEVERITIES = ('high', 'medium')


def _default_points():
    return {'FAIL': {'high': 4, 'medium': 2}, 'UNABLE_TO_ASSESS': {'high': 2, 'medium': 1}}


@dataclass(frozen=True)
class RoutingConfig:
    version: int = 1
    points: dict = field(default_factory=_default_points)
    lane_b_flagged: int = 4
    lane_b_score: int = 10
    slice_size: int = 25
    low_water: int = 10
    lease_seconds: int = 1800
    aging_per_hour: float = 0.5
    ai_per_minute: int = 30
    ai_daily_budget: int = 2000
    on_shift: tuple = ()
    exclusions: tuple = ()


def _int_in(name, value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f'{name} must be a whole number from {low} to {high}')


def _number_in(name, value, low, high):
    if type(value) not in (int, float) or not low <= value <= high:
        raise ValueError(f'{name} must be a number from {low} to {high}')


def _check_points(points):
    if not isinstance(points, dict) or set(points) != set(STATUSES):
        raise ValueError('points must hold exactly FAIL and UNABLE_TO_ASSESS')
    for status in STATUSES:
        row = points[status]
        if not isinstance(row, dict) or set(row) != set(SEVERITIES):
            raise ValueError(f'points for {status} must hold exactly high and medium')
        for severity in SEVERITIES:
            _int_in(f'points {status} {severity}', row[severity], 0, 100)
    for severity in SEVERITIES:
        if points['FAIL'][severity] < points['UNABLE_TO_ASSESS'][severity]:
            raise ValueError(f'a failed {severity} finding must not score less than an unassessable one')


def _check_on_shift(on_shift):
    if not isinstance(on_shift, tuple) or not all(type(b) is str and b for b in on_shift):
        raise ValueError('on_shift must be a tuple of non-empty badge ids')
    if len(set(on_shift)) != len(on_shift):
        raise ValueError('on_shift must not repeat a badge')


def _check_exclusions(exclusions):
    if not isinstance(exclusions, tuple):
        raise ValueError('exclusions must be a tuple of (badge, patient) pairs')
    for pair in exclusions:
        if not (isinstance(pair, tuple) and len(pair) == 2 and all(type(x) is str and x for x in pair)):
            raise ValueError('each exclusion must be a (badge, patient) pair of non-empty text')


def validate(cfg):
    """Return cfg unchanged, or raise ValueError naming the first problem."""
    if not isinstance(cfg, RoutingConfig):
        raise ValueError('not a routing configuration')
    _int_in('version', cfg.version, 1, 10 ** 9)
    _check_points(cfg.points)
    _int_in('lane_b_flagged', cfg.lane_b_flagged, 1, 15)
    _int_in('lane_b_score', cfg.lane_b_score, 1, 1000)
    _int_in('slice_size', cfg.slice_size, 1, 200)
    _int_in('low_water', cfg.low_water, 1, 200)
    if cfg.low_water > cfg.slice_size:
        raise ValueError('low_water must not exceed slice_size')
    _int_in('lease_seconds', cfg.lease_seconds, 60, 86400)
    _number_in('aging_per_hour', cfg.aging_per_hour, 0, 100)
    _int_in('ai_per_minute', cfg.ai_per_minute, 0, 600)
    _int_in('ai_daily_budget', cfg.ai_daily_budget, 0, 100000)
    _check_on_shift(cfg.on_shift)
    _check_exclusions(cfg.exclusions)
    return cfg


DEFAULT = validate(RoutingConfig())
_NAMES = tuple(f.name for f in fields(RoutingConfig))


def to_doc(cfg):
    """Plain data for storage and the audit log (lists, not tuples)."""
    validate(cfg)
    doc = {n: getattr(cfg, n) for n in _NAMES}
    doc['points'] = {s: dict(r) for s, r in cfg.points.items()}
    doc['on_shift'] = list(cfg.on_shift)
    doc['exclusions'] = [list(p) for p in cfg.exclusions]
    return doc


def from_doc(doc):
    if not isinstance(doc, dict) or set(doc) - set(_NAMES):
        raise ValueError('unknown routing configuration field')
    values = dict(doc)
    if 'points' in values:
        values['points'] = {s: dict(r) for s, r in values['points'].items()} if isinstance(values['points'], dict) else values['points']
    if 'on_shift' in values and isinstance(values['on_shift'], list):
        values['on_shift'] = tuple(values['on_shift'])
    if 'exclusions' in values and isinstance(values['exclusions'], list):
        values['exclusions'] = tuple(tuple(p) if isinstance(p, list) else p for p in values['exclusions'])
    return validate(RoutingConfig(**values))
