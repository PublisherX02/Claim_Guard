"""An independent reference implementation of the eight extension rules, written from the rule table in the design spec.

It shares no code with src/extension_rules.py (no fact tags, no YARA, no rule_view, no claim_history): it works on plain dicts with
the simplest loops that express each sentence of the spec, and receives the *whole* list of known claims so it also re-derives which
claims are "earlier". It assumes well-typed input (strings, ints and None); malformed input is the fuzz suite's job.
"""
import datetime
import re

PRIMARY, COMPONENT, NEVER, RX = 'SVC-EXT-PRIMARY', 'SVC-EXT-COMPONENT', 'SVC-EXT-NEVER', 'SVC-EXT-RX'
PAIRS = ((PRIMARY, COMPONENT, True), (PRIMARY, NEVER, False))
EXCEPTION = {'EDU-SEPARATE'}
ROUTES = {'EDU-ROUTE-ORAL', 'EDU-ROUTE-IV', 'EDU-ROUTE-TOPICAL'}
NEEDS_DATE, SECONDARY = 'DX-EXT-ACCIDENT', 'DX-EXT-SECONDARY'
LIMITS = {'SVC-CONSULT': 1, 'SVC-LAB': 3, 'SVC-IMAGE': 1, 'SVC-THERAPY': 4, 'SVC-DENTAL': 2, 'SVC-PHARM': 10}


def day(text):
    if not isinstance(text, str) or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}', text):
        return None
    try:
        return datetime.date(int(text[:4]), int(text[5:7]), int(text[8:]))
    except ValueError:
        return None


def number(v):
    return v if isinstance(v, int) and not isinstance(v, bool) else None


def text(v):
    return v if isinstance(v, str) and v else None


def rows(c):
    return [l for l in (c.get('lines') or []) if isinstance(l, dict)]


def verdict(fail, unknown, applicable=True):
    if fail:
        return 'FAIL'
    if unknown:
        return 'UNABLE_TO_ASSESS'
    return 'PASS' if applicable else 'NOT_APPLICABLE'


def pair_rules(c):
    lines = rows(c)
    result = {}
    for rid, only_never in (('E001', False), ('E002', True)):
        fail, unknown, applicable = set(), False, False
        for primary, secondary, allowed in PAIRS:
            if only_never and allowed:
                continue
            P = [l for l in lines if l.get('service_code') == primary]
            S = [l for l in lines if l.get('service_code') == secondary]
            if not P or not S:
                continue
            applicable = True
            if any(day(l.get('service_date')) is None for l in P + S):
                unknown = True
            for a in P:
                for b in S:
                    if day(a.get('service_date')) is not None and day(a.get('service_date')) == day(b.get('service_date')):
                        excepted = b.get('modifier') in EXCEPTION
                        if excepted if only_never else not excepted:
                            fail |= {a['line_id'], b['line_id']}
        result[rid] = (verdict(fail, unknown, applicable), fail)
    return result


def event_date(c):
    dx = text(c.get('diagnosis_code'))
    if dx is None:
        return 'UNABLE_TO_ASSESS', set()
    if dx != NEEDS_DATE:
        return 'NOT_APPLICABLE', set()
    service = [d for d in (day(l.get('service_date')) for l in rows(c)) if d]
    if not service:
        return 'UNABLE_TO_ASSESS', set()
    bodies = [c.get('notes')] + [a.get('text') for a in (c.get('attachments') or []) if isinstance(a, dict)]
    found = []
    for body in bodies:
        if isinstance(body, str):
            found += [day(m.group(1)) for m in re.finditer(r'Event date: ?([0-9]{4}-[0-9]{2}-[0-9]{2})(?![0-9])', body)]
    ok = any(d is not None and d <= min(service) for d in found)
    return ('PASS' if ok else 'FAIL'), set()


def route(c):
    rx = [l for l in rows(c) if l.get('service_code') == RX]
    if not rx:
        return 'NOT_APPLICABLE', set()
    bad = {l['line_id'] for l in rx if l.get('modifier') not in ROUTES}
    return ('FAIL' if bad else 'PASS'), bad


def primary_dx(c):
    dx = text(c.get('diagnosis_code'))
    if dx is None:
        return 'UNABLE_TO_ASSESS', set()
    return ('FAIL' if dx == SECONDARY else 'PASS'), set()


def earlier(c, everything):
    me = (c.get('submission_date') or '', c.get('claim_id') or '')
    out = []
    for p in everything:
        if not isinstance(p, dict) or not text(p.get('patient_id')) or not text(p.get('claim_id')):
            continue
        if p['patient_id'] == c.get('patient_id') and text(c.get('patient_id')) and p['claim_id'] != c.get('claim_id') \
                and ((p.get('submission_date') or ''), p['claim_id']) < me:
            out.append(p)
    return out


def duplicate(c, prior):
    provider = text(c.get('provider_id'))
    if provider is None or text(c.get('patient_id')) is None:
        return 'UNABLE_TO_ASSESS', set()

    def key(l):
        k = (text(l.get('service_code')), day(l.get('service_date')), number(l.get('quantity')), number(l.get('net_amount')))
        return None if None in k else k

    seen = {key(l) for p in prior if p.get('provider_id') == provider for l in rows(p) if key(l) is not None}
    fail = {l['line_id'] for l in rows(c) if key(l) is not None and key(l) in seen}
    unknown = any(key(l) is None for l in rows(c))
    return verdict(fail, unknown), fail


def authorization(c, prior):
    groups = {}
    for l in rows(c):
        if text(l.get('authorization_id')):
            groups.setdefault(l['authorization_id'], []).append(l)
    if not groups:
        return 'NOT_APPLICABLE', set()
    fail, unknown = set(), False
    for aid, lines in groups.items():
        record = next((a for a in (c.get('authorizations') or []) if isinstance(a, dict) and a.get('authorization_id') == aid), None)
        limit = number(record.get('max_quantity')) if record else None
        used = [number(l.get('quantity')) for l in lines]
        used += [number(l.get('quantity')) for p in prior for l in rows(p) if l.get('authorization_id') == aid]
        if limit is None or None in used:
            unknown = True
        elif sum(used) > limit:
            fail |= {l['line_id'] for l in lines}
    return verdict(fail, unknown), fail


def daily(c, prior):
    groups, unknown = {}, False
    for l in rows(c):
        if l.get('service_code') not in LIMITS:
            continue
        d, q = day(l.get('service_date')), number(l.get('quantity'))
        if d is None or q is None:
            unknown = True
        else:
            groups.setdefault((l['service_code'], d), []).append(l)
    if not groups and not unknown:
        return 'NOT_APPLICABLE', set()
    provider = text(c.get('provider_id'))
    if provider is None or text(c.get('patient_id')) is None:
        return 'UNABLE_TO_ASSESS', set()
    fail = set()
    for (code, d), lines in groups.items():
        before = sum(number(l.get('quantity')) for p in prior if p.get('provider_id') == provider for l in rows(p)
                     if l.get('service_code') == code and day(l.get('service_date')) == d and number(l.get('quantity')) is not None)
        if before > 0 and before + sum(number(l.get('quantity')) for l in lines) > LIMITS[code]:
            fail |= {l['line_id'] for l in lines}
    return verdict(fail, unknown), fail


def oracle(c, everything):
    """{rule_id: (status, set of affected line ids)} for claim c, given every claim known (c may be among them)."""
    out = dict(pair_rules(c))
    out['E003'], out['E004'], out['E005'] = event_date(c), route(c), primary_dx(c)
    prior = earlier(c, everything)
    out['E101'], out['E102'], out['E103'] = duplicate(c, prior), authorization(c, prior), daily(c, prior)
    return out
