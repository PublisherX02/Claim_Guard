"""Triage: where a claim goes, decided by a published formula and written down as a receipt.

A finding is *flagged* when its status is FAIL or UNABLE_TO_ASSESS. Each flagged finding scores points from the routing
configuration (severity-weighted), and the claim lands in one lane:

    green  nothing flagged
    A      one to three flagged, score below the lane-B score
    B      lane_b_flagged or more flagged, or a score at or above lane_b_score

Who may take the claim is separate: any flagged high-severity finding needs claims.decide_high, everything else claims.decide.

The result set is checked before it is trusted. If it is not exactly the 15 official rules, each once, with known statuses and
severities, it is *degraded* and fails closed to lane B with senior eligibility; a damaged result set can never look clean.
"""
import hashlib
import json

FLAGGED = ('FAIL', 'UNABLE_TO_ASSESS')
KNOWN_STATUSES = frozenset({'PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE'})
KNOWN_SEVERITIES = frozenset({'high', 'medium'})
RULE_IDS = tuple('R%03d' % i for i in range(1, 16))


def _canon(value):
    try:
        return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError):
        raise ValueError('value cannot be hashed: it is not plain JSON data') from None


def _sha(value):
    return hashlib.sha256(_canon(value).encode('utf-8')).hexdigest()


def input_hash(claim):
    return _sha(claim)


def _sorted_rows(results):
    rows = [r for r in results if isinstance(r, dict)] if isinstance(results, list) else []
    return sorted(rows, key=lambda r: str(r.get('rule_id')))


def result_hash(results):
    if isinstance(results, list) and all(isinstance(r, dict) for r in results):
        return _sha(_sorted_rows(results))
    return _sha(results)


def facts_hash(results):
    """The engine does not expose its facts blob; the evidence and affected lines it derived from the facts are what a later
    replay compares."""
    return _sha([(r.get('rule_id'), r.get('affected_line_ids'), r.get('evidence')) for r in _sorted_rows(results)])


def degraded(results):
    if not isinstance(results, list) or len(results) != len(RULE_IDS):
        return True
    seen = []
    for r in results:
        if not isinstance(r, dict) or r.get('status') not in KNOWN_STATUSES or r.get('severity') not in KNOWN_SEVERITIES:
            return True
        seen.append(r.get('rule_id'))
    return sorted(map(str, seen)) != list(RULE_IDS)


def flagged(results):
    if not isinstance(results, list):
        return []
    return [r for r in results if isinstance(r, dict) and r.get('status') in FLAGGED]


def _severity(row):
    return row.get('severity') if row.get('severity') in KNOWN_SEVERITIES else 'high'   # unknown counts as the most serious


def score(results, cfg):
    return sum(cfg.points[r['status']][_severity(r)] for r in flagged(results))


def lane(results, cfg):
    if degraded(results):
        return 'B'
    rows = flagged(results)
    if not rows:
        return 'green'
    if len(rows) >= cfg.lane_b_flagged or score(results, cfg) >= cfg.lane_b_score:
        return 'B'
    return 'A'


def eligibility(results):
    if degraded(results) or any(_severity(r) == 'high' for r in flagged(results)):
        return 'decide_high'
    return 'decide'


def make_receipt(claim, results, cfg, rule_pack_hash, engine_version, now):
    statuses = {r['rule_id']: r.get('status') for r in _sorted_rows(results) if isinstance(r.get('rule_id'), str)}
    return {
        'claim_id': claim.get('claim_id') if isinstance(claim, dict) else None,
        'input_hash': input_hash(claim),
        'rule_pack_hash': rule_pack_hash,
        'engine_version': engine_version,
        'facts_hash': facts_hash(results),
        'result_hash': result_hash(results),
        'statuses': statuses,
        'score': score(results, cfg),
        'lane': lane(results, cfg),
        'eligibility': eligibility(results),
        'config_version': cfg.version,
        'degraded': degraded(results),
        'created_at': now,
    }
