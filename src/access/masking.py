"""Data minimization for the reviewer API: what a response may contain depends on the caller's permissions.

* Patient and member identifiers are replaced by stable pseudonyms (a keyed hash, so the same person is the same pseudonym
  everywhere but nothing can be recovered from it). The replacement is applied to every string in the response, so an
  identifier quoted inside an evidence value or an explanation is covered too.
* Free text (the claim notes and attachment text) is removed, not blanked, unless the caller may read it: hide, not disable.
* After masking, the response is checked for any leftover raw identifier; if one is found the request fails instead of leaking.
"""
import copy
import hashlib
import hmac
import json
import re

PREFIXES = ('PAT', 'MEM')
PSEUDONYM_HEX = 8
_ATTACHMENT_TEXT = re.compile(r'\A/attachments/\d+/text\Z')


class RedactionError(Exception):
    """A raw identifier survived masking; the response must not be sent."""


def pseudonym(key, prefix, value):
    if prefix not in PREFIXES or not isinstance(value, str) or not value or not isinstance(key, str) or len(key) < 32:
        raise ValueError('cannot make a pseudonym from this input')
    digest = hmac.new(key.encode('utf-8'), f'{prefix}|{value}'.encode('utf-8'), hashlib.sha256).hexdigest()
    return f'{prefix}-{digest[:PSEUDONYM_HEX].upper()}'


def identifier_map(claim, key):
    """{raw identifier: pseudonym} for every patient or member identifier the claim carries."""
    pairs = []

    def add(value, prefix):
        if isinstance(value, str) and value:
            pairs.append((value, prefix))

    add(claim.get('patient_id'), 'PAT')
    add(claim.get('member_id'), 'MEM')
    coverage = claim.get('coverage')
    if isinstance(coverage, dict):
        add(coverage.get('beneficiary_patient_id'), 'PAT')
        add(coverage.get('member_id'), 'MEM')
    for pool in ('authorizations', 'attachments'):
        for row in claim.get(pool) or []:
            if isinstance(row, dict):
                add(row.get('patient_id'), 'PAT')
    mapping = {}
    for raw, prefix in pairs:
        if raw not in mapping:
            mapping[raw] = pseudonym(key, prefix, raw)
    return mapping


def _pattern(mapping):
    """One case-insensitive alternation, longest identifier first, so a short identifier never eats part of a longer one."""
    return re.compile('|'.join(re.escape(raw) for raw in sorted(mapping, key=len, reverse=True)), re.IGNORECASE)


def scrub(obj, mapping):
    """A deep copy of obj with every raw identifier in every string replaced by its pseudonym."""
    if not mapping:
        return copy.deepcopy(obj)
    lookup = {raw.casefold(): masked for raw, masked in mapping.items()}
    pattern = _pattern(mapping)

    def walk(o):
        if isinstance(o, str):
            return pattern.sub(lambda m: lookup.get(m.group(0).casefold(), m.group(0)), o)
        if isinstance(o, dict):
            return {k: walk(v) for k, v in o.items()}
        if isinstance(o, list):
            return [walk(v) for v in o]
        if isinstance(o, tuple):
            return tuple(walk(v) for v in o)
        return o
    return walk(obj)


def leaks(obj, raw_ids):
    """The raw identifiers that appear anywhere in obj."""
    text = json.dumps(obj, ensure_ascii=False, default=str)
    return {raw for raw in raw_ids if re.search(re.escape(raw), text, re.IGNORECASE)}


def _strip_free_text(o):
    if isinstance(o, dict):
        return {k: _strip_free_text(v) for k, v in o.items() if k not in ('notes', 'text')}
    if isinstance(o, list):
        return [_strip_free_text(v) for v in o]
    return o


def _is_free_text_path(path):
    return isinstance(path, str) and (path == '/notes' or path.startswith('/notes/') or _ATTACHMENT_TEXT.match(path) is not None)


def shape_claim(claim, results, permissions, key, unmasked=False, verify=True):
    mapping = identifier_map(claim, key)
    claim_out, results_out = copy.deepcopy(claim), copy.deepcopy(results)
    if 'claims.view_notes' not in permissions:
        claim_out.pop('notes', None)
        for attachment in claim_out.get('attachments') or []:
            if isinstance(attachment, dict):
                attachment.pop('text', None)
        for result in results_out:
            kept = []
            for evidence in result.get('evidence') or []:
                if isinstance(evidence, dict) and _is_free_text_path(evidence.get('path')):
                    continue
                if isinstance(evidence, dict) and 'value' in evidence:
                    evidence = {**evidence, 'value': _strip_free_text(evidence['value'])}
                kept.append(evidence)
            result['evidence'] = kept
    if not unmasked:
        claim_out, results_out = scrub(claim_out, mapping), scrub(results_out, mapping)
        if verify and leaks({'claim': claim_out, 'results': results_out}, mapping):
            raise RedactionError('an identifier survived masking')
    return {'claim': claim_out, 'results': results_out}
