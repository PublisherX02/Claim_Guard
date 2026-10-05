"""An independent implementation of the routing formula, written from the specification table with plain loops.

It deliberately imports nothing from src/workqueue: if the two implementations ever disagree, one of them is wrong.
"""
POINTS = {('FAIL', 'high'): 4, ('FAIL', 'medium'): 2, ('UNABLE_TO_ASSESS', 'high'): 2, ('UNABLE_TO_ASSESS', 'medium'): 1}
RULE_IDS = ['R%03d' % i for i in range(1, 16)]


def _degraded(results):
    if not isinstance(results, list) or len(results) != 15:
        return True
    seen = []
    for r in results:
        if not isinstance(r, dict):
            return True
        if r.get('status') not in ('PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE'):
            return True
        if r.get('severity') not in ('high', 'medium'):
            return True
        seen.append(r.get('rule_id'))
    return sorted(seen) != RULE_IDS


def oracle(results, points=None, lane_b_flagged=4, lane_b_score=10):
    """Return (score, lane, eligibility, degraded)."""
    table = dict(POINTS) if points is None else {(s, v): p for s, row in points.items() for v, p in row.items()}
    if _degraded(results):
        return (0, 'B', 'decide_high', True)
    total, count, high = 0, 0, False
    for r in results:
        if r['status'] in ('FAIL', 'UNABLE_TO_ASSESS'):
            count += 1
            total += table[(r['status'], r['severity'])]
            high = high or r['severity'] == 'high'
    if count == 0:
        lane = 'green'
    elif count >= lane_b_flagged or total >= lane_b_score:
        lane = 'B'
    else:
        lane = 'A'
    return (total, lane, 'decide_high' if high else 'decide', False)
