"""Where the reviewer API reads claims and rule results from.

For now this is a read-only view over the claims and the engine's results held in memory (loaded from the existing JSONL
files); moving them into the database belongs to the task-queue work. Every accessor validates its inputs and returns copies,
so a caller can neither reach a path through a claim id nor change the stored data.
"""
import copy
import re
from collections import Counter

STATUSES = ('PASS', 'FAIL', 'UNABLE_TO_ASSESS', 'NOT_APPLICABLE')
FLAGGED = ('FAIL', 'UNABLE_TO_ASSESS')
SEVERITY_ORDER = {'medium': 1, 'high': 2}
_CLAIM_ID = re.compile(r'\A[A-Za-z0-9_-]{1,64}\Z')
_RULE_ID = re.compile(r'\AR\d{3}\Z')
MAX_PAGE = 200


class FileClaimStore:
    def __init__(self, claims, results_by_claim):
        self._claims = {c['claim_id']: c for c in claims}
        self._results = results_by_claim

    @classmethod
    def from_jsonl(cls, claims_path, cfg):
        from engine_core import load_jsonl
        from yara_engine import evaluate
        claims = load_jsonl(claims_path)
        return cls(claims, {c['claim_id']: evaluate(c, cfg, []) for c in claims})

    def summaries(self, limit=50, offset=0, status=None, rule_id=None):
        if type(limit) is not int or not 1 <= limit <= MAX_PAGE or type(offset) is not int or offset < 0:
            raise ValueError(f'limit must be 1 to {MAX_PAGE} and offset 0 or more')
        if status is not None and (not isinstance(status, str) or status not in STATUSES):
            raise ValueError('unknown status')
        if rule_id is not None and (not isinstance(rule_id, str) or _RULE_ID.match(rule_id) is None):
            raise ValueError('rule_id must look like R009')
        rows = []
        for claim_id in self._claims:
            results = self._results[claim_id]
            if rule_id is not None:
                chosen = [r for r in results if r['rule_id'] == rule_id]
                if not any((r['status'] == status) if status else (r['status'] in FLAGGED) for r in chosen):
                    continue
            elif status is not None and not any(r['status'] == status for r in results):
                continue
            flagged = [r for r in results if r['status'] in FLAGGED]
            top = max((r['severity'] for r in flagged), key=lambda s: SEVERITY_ORDER.get(s, 0), default=None)
            rows.append({'claim_id': claim_id, 'counts': dict(Counter(r['status'] for r in results)),
                         'max_severity': top, 'flagged': bool(flagged)})
        return rows[offset:offset + limit]

    def get(self, claim_id):
        if not isinstance(claim_id, str) or _CLAIM_ID.match(claim_id) is None or claim_id not in self._claims:
            return None
        return copy.deepcopy(self._claims[claim_id]), copy.deepcopy(self._results[claim_id])
