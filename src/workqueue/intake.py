"""Intake: validate a claim, run the engine, write the triage receipt, and store everything in one atomic write.

The same claim submitted twice is stored once and returns the same receipt. A changed body under an existing claim id becomes the
next version with its own receipt. The receipt is also written to the security log, so what the system told the submitter is
on the audit trail. The advisory (extension) rules are added in a later step and never reach the routing numbers.
"""
import json
import re

from engine_core import validate_transport

from . import routing_config as rc
from . import shadow, triage
from .store import check_text

CLAIM_ID = re.compile(r'\A[A-Za-z0-9_-]{1,64}\Z')
RETRIES = 3


def check_claim(claim):
    """The transport check the rest of the system uses, plus a claim id that is safe as a key and a file name."""
    validate_transport(claim)
    if not isinstance(claim['claim_id'], str) or CLAIM_ID.match(claim['claim_id']) is None:
        raise ValueError('claim_id must be 1 to 64 letters, digits, underscore or hyphen')


class Intake:
    def __init__(self, store, engine, securitylog, clock, rule_pack_hash, engine_version, history=None, advisory=None):
        """engine(claim) -> the 15 rule results. history has earlier_claims(claim) (see claim_history); advisory(claim, history) ->
        the extension results (defaults to extension_rules.evaluate_extensions). Advisory results are stored beside the official
        ones and never reach the routing numbers."""
        self.store, self.engine, self.log, self.clock = store, engine, securitylog, clock
        self.history, self._advisory_fn = history, advisory
        self.rule_pack_hash, self.engine_version = rule_pack_hash, engine_version

    def routing_config(self):
        doc = self.store.latest_config()
        return rc.DEFAULT if doc is None else rc.from_doc(doc)

    def submit(self, claim):
        if not isinstance(claim, dict):
            raise ValueError('a claim must be an object')
        try:
            check_claim(claim)
        except (ValueError, TypeError, KeyError) as e:
            raise ValueError(f'claim refused: {e}') from None
        claim_id, ihash = claim['claim_id'], triage.input_hash(claim)
        cfg = self.routing_config()
        for _ in range(RETRIES):
            found = self._receipt_for(claim_id, ihash)
            if found is not None:
                return found
            latest = self.store.get(claim_id)
            version = 1 if latest is None else latest['version'] + 1
            results = self.engine(claim)
            receipt = triage.make_receipt(claim, results, cfg, self.rule_pack_hash, self.engine_version, self.clock())
            doc = {'claim_id': claim_id, 'version': version, 'input_hash': ihash, 'claim': claim, 'results': results, 'receipt': receipt,
                   'advisory': self._advisory(claim)}
            if self.store.put_triaged(doc):
                self._shadow(claim_id, version, receipt)
                self.log.record('triage_receipt', claim_id=claim_id, input_hash=ihash, result_hash=receipt['result_hash'],
                                lane=receipt['lane'], score=receipt['score'], config_version=receipt['config_version'])
                return receipt
        raise RuntimeError('the claim could not be stored: concurrent submissions kept taking its version')

    def _shadow(self, claim_id, version, receipt):
        """Record what an automatic clearer would have done. It is stored beside the claim and acts on nothing; a failure is ignored."""
        try:
            shadow.record(self.store, claim_id, version, shadow.predict_clear(receipt), receipt['created_at'])
        except Exception:  # noqa: BLE001 - shadow mode must never affect a claim
            pass

    def _advisory(self, claim):
        """The extension (advisory) results, or [] if they cannot be produced: they must never stop a claim from being triaged."""
        if self.history is None and self._advisory_fn is None:
            return []
        try:
            if self._advisory_fn is not None:
                rows = self._advisory_fn(claim, self.history)
            else:
                from extension_rules import evaluate_extensions
                rows = evaluate_extensions(claim, self.history)
            return json.loads(json.dumps(list(rows)))              # plain data only, so it can be stored anywhere
        except Exception:  # noqa: BLE001 - advisory only
            return []

    def _receipt_for(self, claim_id, ihash):
        """The receipt of any stored version of this claim with this exact body under the current rule pack, or None."""
        check_text(claim_id, 'claim_id')
        latest = self.store.get(claim_id)
        if latest is None:
            return None
        for version in range(latest['version'], 0, -1):
            doc = latest if version == latest['version'] else self.store.get(claim_id, version)
            if doc is not None and doc['input_hash'] == ihash and doc['receipt'].get('rule_pack_hash') == self.rule_pack_hash:
                return doc['receipt']
        return None
