"""Earlier claims of the same patient, read from the queue store, for the extension rules that look across claims (E101 to E103).

It answers exactly what claim_history.InMemoryHistory answers for the same data (same definition of "earlier": same patient,
another claim id, strictly before by (submission date, claim id)), so a rule gives the same verdict on a batch file and on the
database. Works over any QueueStore, in memory or MongoDB.
"""
import claim_history


class StoreHistory:
    def __init__(self, store):
        self._store = store

    def earlier_claims(self, claim):
        pid = claim.get('patient_id') if isinstance(claim, dict) else None
        if not isinstance(pid, str) or not pid:
            return []
        me, mine = claim_history.order_key(claim), claim.get('claim_id')
        if me is None:
            return []
        rows = [c for c in self._store.claims_for_patient(pid)
                if isinstance(c.get('claim_id'), str) and c['claim_id'] and c['claim_id'] != mine
                and claim_history.order_key(c) is not None and claim_history.order_key(c) < me]
        rows.sort(key=claim_history.order_key)
        return rows
