"""Intake: validate a claim, run the engine, write the triage receipt, and store everything in one atomic write.

The same claim submitted twice is stored once and returns the same receipt. A changed body under an existing claim id becomes the
next version with its own receipt. The receipt is also written to the security log, so what the system told the submitter is
on the audit trail. The advisory (extension) rules are added in a later step and never reach the routing numbers.
"""
import re

from engine_core import validate_transport

from . import routing_config as rc
from . import triage
from .store import check_text

CLAIM_ID = re.compile(r'\A[A-Za-z0-9_-]{1,64}\Z')
RETRIES = 3


def check_claim(claim):
    """The transport check the rest of the system uses, plus a claim id that is safe as a key and a file name."""
    validate_transport(claim)
    if not isinstance(claim['claim_id'], str) or CLAIM_ID.match(claim['claim_id']) is None:
        raise ValueError('claim_id must be 1 to 64 letters, digits, underscore or hyphen')


class Intake:
    def __init__(self, store, engine, securitylog, clock, rule_pack_hash, engine_version):
        """engine(claim) -> the 15 rule results."""
        self.store, self.engine, self.log, self.clock = store, engine, securitylog, clock
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
            doc = {'claim_id': claim_id, 'version': version, 'input_hash': ihash, 'claim': claim, 'results': results, 'receipt': receipt}
            if self.store.put_triaged(doc):
                self.log.record('triage_receipt', claim_id=claim_id, input_hash=ihash, result_hash=receipt['result_hash'],
                                lane=receipt['lane'], score=receipt['score'], config_version=receipt['config_version'])
                return receipt
        raise RuntimeError('the claim could not be stored: concurrent submissions kept taking its version')

    def _receipt_for(self, claim_id, ihash):
        """The receipt of any stored version of this claim with this exact body, or None."""
        check_text(claim_id, 'claim_id')
        latest = self.store.get(claim_id)
        if latest is None:
            return None
        for version in range(latest['version'], 0, -1):
            doc = latest if version == latest['version'] else self.store.get(claim_id, version)
            if doc is not None and doc['input_hash'] == ihash:
                return doc['receipt']
        return None
