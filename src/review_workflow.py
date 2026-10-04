"""Human review workflow: validate reviewer decisions against the real current
findings, record them in the audit chain, report unresolved counts, and run a
corrected-claim recheck as a NEW version and NEW run.

Principles (docs/05_Architecture_and_AI.md, docs/01 scope boundaries)
--------------------------------------------------------------------
- Decisions never mutate rule results. They are separate audit events that
  reference the finding; the result records stay exactly as the engine
  produced them.
- A decision must match reality: its `original_status` has to equal the
  finding's actual status, and only FAIL / UNABLE_TO_ASSESS findings can be
  reviewed (a PASS or NOT_APPLICABLE finding has nothing to confirm or dismiss).
- "Corrections create a new version and run." A recheck never edits the original
  claim; it runs the corrected claim through review_package() again (new
  run_id, new input_hash) and links the two runs in the audit log.
- Nothing here approves, denies or submits a claim.
"""
import argparse
import copy
import json
import threading
from datetime import datetime, timezone
from pathlib import Path

import uuid

from audit_log import AuditLog, audited_review
from claim_review import IngestionError, input_hash, validate_input
from schema_subset import validate as validate_schema

ROOT = Path(__file__).resolve().parents[1]
REVIEWABLE = {'FAIL', 'UNABLE_TO_ASSESS'}
RESOLVING = {'confirm_issue', 'dismiss_with_reason'}
PENDING = {'request_information', 'mark_corrected_for_recheck'}
_SCHEMA = json.loads((ROOT / 'schemas' / 'review_event.schema.json').read_text(encoding='utf-8'))


class DecisionError(ValueError):
    pass


def load_decisions(path):
    """Read the JSONL the review page downloads (review_decisions.jsonl)."""
    return [json.loads(l) for l in Path(path).read_text(encoding='utf-8').split('\n') if l.strip()]


def index_findings(results):
    return {(r['claim_id'], r['rule_id']): r for r in results}


def validate_decision(decision, findings):
    """Raise DecisionError unless this decision is well-formed AND refers to a
    real, reviewable finding whose status matches what the reviewer saw."""
    try:
        validate_schema(decision, _SCHEMA)
    except ValueError as e:
        raise DecisionError(f'Decision does not match review_event schema: {e}') from e
    if not decision['actor'].strip():
        raise DecisionError('actor is required')
    if not decision['reason'].strip():
        raise DecisionError('A reason is required for every decision')
    try:
        datetime.fromisoformat(decision['created_at'].replace('Z', '+00:00'))
    except ValueError as e:
        raise DecisionError('created_at is not an ISO-8601 timestamp') from e
    finding = findings.get((decision['claim_id'], decision['rule_id']))
    if finding is None:
        raise DecisionError(f"No finding for {decision['claim_id']}/{decision['rule_id']}")
    if finding['status'] not in REVIEWABLE:
        raise DecisionError(f"{decision['rule_id']} is {finding['status']}; only "
                            f"{sorted(REVIEWABLE)} findings can be reviewed")
    if decision['original_status'] != finding['status']:
        raise DecisionError(f"original_status {decision['original_status']!r} does not match the "
                            f"finding's actual status {finding['status']!r}")
    return decision


def apply_decisions(log, decisions, results):
    """Validate the whole batch first, then write it in one append. A single bad
    decision rejects the batch: a partially recorded review is worse than none."""
    findings = index_findings(results)
    for d in decisions:
        validate_decision(d, findings)
    return log.append_review_decisions(decisions)


_LOG_CACHE = {}
_LOG_CACHE_LOCK = threading.Lock()


def _parse_log(log_path):
    """The log's rows. The log is append-only, so a file whose size and modification time are unchanged parses to the same rows:
    the parsed result is kept per file and re-read only when either changes (the reviewer API calls this on every request)."""
    path = Path(log_path)
    try:
        st = path.stat()
    except FileNotFoundError:
        return []
    stamp = (st.st_size, st.st_mtime_ns)
    key = str(path.resolve())
    with _LOG_CACHE_LOCK:
        hit = _LOG_CACHE.get(key)
        if hit and hit[0] == stamp:
            return hit[1]
    rows = [json.loads(l) for l in path.read_text(encoding='utf-8').split('\n') if l.strip()]
    with _LOG_CACHE_LOCK:
        if len(_LOG_CACHE) > 16:
            _LOG_CACHE.clear()
        _LOG_CACHE[key] = (stamp, rows)
    return rows


def review_state(results, log_path):
    """Per-finding queue state derived from the audit log (the log is the source
    of truth; results are never edited). `results` should be each claim's latest
    run. A decision counts only if it was recorded AFTER that claim's latest
    run_started event, so a finding that is still failing after a recheck goes
    back to 'unreviewed' instead of inheriting the old run's decision. Within a
    run the latest decision wins."""
    rows = _parse_log(log_path)
    run_start = {}
    for row in rows:
        e = row['event']
        if e.get('event_type') == 'run_started':
            run_start[e['claim_id']] = row['sequence']
    latest = {}
    for row in rows:
        e = row['event']
        if 'action' in e and row['sequence'] > run_start.get(e['claim_id'], 0):
            latest[(e['claim_id'], e['rule_id'])] = e['action']
    state = {}
    for r in results:
        if r['status'] not in REVIEWABLE:
            continue
        action = latest.get((r['claim_id'], r['rule_id']))
        state[(r['claim_id'], r['rule_id'])] = (
            'unreviewed' if action is None else
            'resolved' if action in RESOLVING else 'awaiting_follow_up')
    return state


def unresolved_counts(results, log_path):
    state = review_state(results, log_path)
    counts = {'unreviewed': 0, 'awaiting_follow_up': 0, 'resolved': 0}
    for v in state.values():
        counts[v] += 1
    counts['unresolved_total'] = counts['unreviewed'] + counts['awaiting_follow_up']
    return counts


def recheck(log, original_claim, corrected_claim, prior_results, prior_trace, cfg,
            rule_ids, actor, reason, provider=None, fallback=None):
    """Record a `mark_corrected_for_recheck` decision for each rule the reviewer
    says was corrected, then re-run the corrected claim as a new version.

    Returns (new_results, new_ai, new_trace, changes). The original claim and
    prior results are never modified. `changes` maps each rechecked rule to
    (old_status, new_status) so 'corrected' is verified, not assumed.
    """
    if corrected_claim['claim_id'] != original_claim['claim_id']:
        raise DecisionError('A corrected claim must keep the original claim_id')
    if corrected_claim == original_claim:
        raise DecisionError('Corrected claim is identical to the original; nothing was corrected')
    findings = index_findings(prior_results)
    now = datetime.now(timezone.utc).isoformat()
    decisions = [{'claim_id': original_claim['claim_id'], 'rule_id': rid, 'action': 'mark_corrected_for_recheck',
                  'actor': actor, 'reason': reason, 'created_at': now,
                  'original_status': findings[(original_claim['claim_id'], rid)]['status']}
                 for rid in rule_ids if (original_claim['claim_id'], rid) in findings]
    if len(decisions) != len(rule_ids):
        raise DecisionError('Recheck requested for a rule that has no prior finding')
    for d in decisions:
        validate_decision(d, findings)

    try:
        validate_input(copy.deepcopy(corrected_claim))
    except IngestionError as e:
        raise DecisionError(f'Corrected claim failed ingestion: {e}') from e
    new_hash = input_hash(corrected_claim)
    if new_hash == prior_trace['input_hash']:
        raise DecisionError('Corrected claim hashes identically to the original')
    new_run_id = str(uuid.uuid4())

    # Write-ahead: the decisions and the run link are recorded BEFORE the new run starts.
    log.append_review_decisions(decisions)
    log.append_system_events([{
        'event_type': 'recheck_run', 'claim_id': original_claim['claim_id'],
        'prior_run_id': prior_trace['run_id'], 'new_run_id': new_run_id,
        'prior_input_hash': prior_trace['input_hash'], 'new_input_hash': new_hash,
        'rechecked_rule_ids': list(rule_ids), 'requested_by': actor,
    }])
    new_results, new_ai, new_trace = audited_review(
        log, copy.deepcopy(corrected_claim), cfg, provider=provider, fallback=fallback,
        source_format='reviewer_correction', run_id=new_run_id)
    new_by_rule = index_findings(new_results)
    changes = {rid: (findings[(original_claim['claim_id'], rid)]['status'],
                     new_by_rule[(original_claim['claim_id'], rid)]['status']) for rid in rule_ids}
    return new_results, new_ai, new_trace, changes


def main():
    p = argparse.ArgumentParser(description='Validate and record reviewer decisions from the review page.')
    p.add_argument('--results', required=True, help='engine results JSONL (run_yara.py output)')
    p.add_argument('--decisions', required=True, help='review_decisions.jsonl downloaded from the review page')
    p.add_argument('--log', default='outputs/audit.jsonl')
    a = p.parse_args()
    results = [json.loads(l) for l in Path(a.results).read_text(encoding='utf-8').split('\n') if l.strip()]
    log = AuditLog(a.log)
    head, count = apply_decisions(log, load_decisions(a.decisions), results)
    counts = unresolved_counts(results, a.log)
    print(f'Recorded decisions. Chain: {count} events; head {head}.')
    print('Queue:', json.dumps(counts))


if __name__ == '__main__':
    main()
