"""Earlier claims for the extension rules that look across claims (E101 to E103).

"Earlier" is defined once, here: same non-empty patient id, a different claim id, and ordered strictly before the claim by
(submission_date, claim_id). A claim whose submission date is missing or not a real YYYY-MM-DD day cannot be placed in that order:
it is never anyone's earlier claim, and it has no earlier claims of its own (the rules that need the order report UNABLE_TO_ASSESS
for it). That total order makes a claim's result independent of how many later claims exist, so a batch run is
reproducible, and exactly one of two claims with equal dates counts as the earlier. A corrected resubmission that reuses a claim id is
never its own history. The queue work replaces InMemoryHistory with a database-backed class that has the same method.
"""
import copy
import datetime
import re

_ISO_DAY = re.compile(r'[0-9]{4}-[0-9]{2}-[0-9]{2}')


def _text(value):
    return value if isinstance(value, str) else ''


def _day(value):
    if not isinstance(value, str) or not _ISO_DAY.fullmatch(value):
        return None
    try:
        return datetime.date(int(value[:4]), int(value[5:7]), int(value[8:]))
    except ValueError:
        return None


def order_key(claim):
    """(day, claim id), or None when the submission date cannot be read."""
    day = _day(claim.get('submission_date'))
    return None if day is None else (day, _text(claim.get('claim_id')))


class InMemoryHistory:
    def __init__(self, claims):
        self._by_patient = {}
        for c in claims:
            if not isinstance(c, dict) or not isinstance(c.get('patient_id'), str) or not c['patient_id']:
                continue
            if not isinstance(c.get('claim_id'), str) or not c['claim_id'] or order_key(c) is None:
                continue
            self._by_patient.setdefault(c['patient_id'], []).append(copy.deepcopy(c))
        for rows in self._by_patient.values():
            rows.sort(key=order_key)

    def earlier_claims(self, claim):
        pid = claim.get('patient_id') if isinstance(claim, dict) else None
        if not isinstance(pid, str) or not pid:
            return []
        me, mine = order_key(claim), claim.get('claim_id')
        if me is None:
            return []
        return [copy.deepcopy(c) for c in self._by_patient.get(pid, []) if c['claim_id'] != mine and order_key(c) < me]
