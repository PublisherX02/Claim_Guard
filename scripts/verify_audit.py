"""Verify an audit log and, optionally, that it matches a results file.

    python scripts/verify_audit.py --log outputs/audit_dev/audit.jsonl \
        [--results outputs/yara_dev_predictions.jsonl]

1. Hash chain intact and consistent with the anchor file (truncation / replacement), and no rows after the
   anchored position (rows appended outside the writer; pass --allow-unanchored for a log still being written).
2. AI ordering: every AI action was registered (ai_request: the question, the deterministic
   verdict, finding + prompt hashes, action type) BEFORE the model was called, answered once, and
   classified human_escalation; auto_correct never appears.
3. With --results: every result's digest equals the result_hash of a rule_check event logged for
   that claim and rule (any run: a rechecked claim legitimately has more than one). The log therefore cannot silently disagree with the
   results file it claims to describe.
"""
import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from audit import digest
from audit_log import anchor_status, verify_ai_ordering, verify_with_anchor


def timestamp_report(a):
    """Print the timestamp state (separate from the chain state). Returns False when it fails the stated requirement."""
    import timestamp_anchor
    anchor = Path(a.log).with_name(Path(a.log).name + '.head.json')
    ca = Path(a.tsa_ca).read_bytes() if a.tsa_ca else None
    st = timestamp_anchor.timestamp_status(a.log, anchor, ca)
    msg = {'none': 'Timestamp: none (the log was never stamped by a Time-Stamp Authority).',
           'unverified': 'Timestamp: a token exists but was NOT checked (no --tsa-ca given).',
           'current': f'Timestamp: verified, covers the whole log; signed at {st.get("gen_time")} by {st.get("tsa_subject")}.',
           'stale': f'Timestamp: verified but {st.get("unstamped_events")} newer event(s) are not yet stamped (token from {st.get("gen_time")}).',
           'invalid': f'Timestamp: INVALID: {st.get("reason")}'}[st['state']]
    print(msg)
    if st['state'] == 'invalid':
        return False
    return st['state'] == 'current' or not a.require_timestamp


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--log', required=True)
    p.add_argument('--results')
    p.add_argument('--allow-unanchored', action='store_true',
                   help='do not fail on rows after the anchored position (only for a log a writer is still appending to)')
    p.add_argument('--require-key', action='store_true',
                   help='exit non-zero unless the anchor is HMAC-signed and AUDIT_ANCHOR_KEY is set here to verify it')
    p.add_argument('--tsa-ca', help='PEM file with the CA(s) allowed to issue the TSA certificate: also check the RFC 3161 timestamp '
                   '(reported separately from the chain; see scripts/timestamp_log.py)')
    p.add_argument('--require-timestamp', action='store_true',
                   help='exit non-zero unless a verified timestamp token covers the anchor (needs --tsa-ca); a stale token fails too')
    a = p.parse_args(argv)
    head, count = verify_with_anchor(a.log, strict=not a.allow_unanchored)
    print(f'Chain OK: {count} events, matches anchor exactly; head {head}')
    status = anchor_status(a.log)
    if status['anchor_signed'] and status['key_configured']:
        print('Anchor: HMAC-signed and verified with the configured key.')
    elif status['anchor_signed']:
        print('NOTE: the anchor is signed but AUDIT_ANCHOR_KEY is not set here, so the signature was NOT checked.')
    else:
        print('WARNING: the anchor is NOT signed (AUDIT_ANCHOR_KEY was not set when the log was written). '
              'Whoever can write the log can rewrite it and its anchor together and still pass this check (docs/20, F4).')
    if a.require_key and not (status['anchor_signed'] and status['key_configured']):
        raise SystemExit('--require-key: the anchor is not HMAC-signed and verified with a configured AUDIT_ANCHOR_KEY')
    ts_ok = timestamp_report(a)
    stats = verify_ai_ordering(a.log)
    print('AI ordering OK:', json.dumps(stats))
    if not ts_ok:
        raise SystemExit('--require-timestamp: no verified timestamp covers the anchor (or the timestamp is invalid)')
    if not a.results:
        return
    results = {}
    for line in Path(a.results).read_text(encoding='utf-8').splitlines():
        if line.strip():
            r = json.loads(line)
            results[(r['claim_id'], r['rule_id'])] = digest(r)
    # A result must match the hash of SOME logged run of that claim/rule. A claim that was
    # rechecked has several runs in the log (original and corrected); each is a genuine record.
    logged = {}
    for line in Path(a.log).read_text(encoding='utf-8').splitlines():
        e = json.loads(line)['event']
        if e.get('event_type') == 'rule_check':
            logged.setdefault((e['claim_id'], e['rule_id']), set()).add(e['result_hash'])
    mismatched, missing = [], []
    for key, h in results.items():
        hashes = logged.get(key)
        if not hashes:
            missing.append(key)
        elif h not in hashes:
            mismatched.append(key)
    print(f'Results file: {len(results)} results; missing from log {len(missing)}; hash mismatches {len(mismatched)}')
    if missing or mismatched:
        sys.exit(1)
    print('Every result is recorded in the log with a matching hash.')


if __name__ == '__main__':
    main()
