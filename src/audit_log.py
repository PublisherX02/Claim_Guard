"""Audit log engine: a tamper-evident, append-only record of every check, AI
recommendation, system decision and human review action.

Design
------
- Same row format and hash function as the supplied src/audit.py
  ({sequence, recorded_at, previous_hash, event, hash}), so audit.verify()
  validates a log written here and vice versa. audit.py stays untouched.
- Reviewer decisions keep going through audit.append() (strict, unchanged).
  This module adds the *system* event kinds audit.py cannot express.
- The head hash and event count are also written to a separate anchor file
  (<log>.head.json). verify_with_anchor() then detects truncation and whole-log
  replacement, which a chain alone cannot. Opening an existing log re-checks it
  against its anchor first.

Write-ahead ordering (what is recorded BEFORE the AI acts)
----------------------------------------------------------
audited_review() writes each stage to the log before the next stage runs:

    ingestion + run_started        input accepted
    rule_check x15                 the deterministic verdicts, before any AI call
    ai_request                     the question put to the AI: which finding, its
                                   verdict (violation_detected / cannot_determine),
                                   hashes of the finding and the exact prompt, and
                                   the action type -- written BEFORE the model call
    ai_recommendation | ai_failure the outcome, linked by request_id
    system_decision, run_finished

If a log write fails, the exception propagates and the AI is not called (a hook
failure aborts the run). verify_ai_ordering() re-checks this from the log alone.

AI action types
---------------
Every AI action is recorded as `human_escalation`: the model drafts an explanation and
the finding goes to a person. `auto_correct` (the AI changing a claim or a result) is not
a permitted value; an event claiming it is rejected at write time and by the verifier.

What this is NOT
----------------
A hash chain is tamper-EVIDENT, not immutable: anyone with write access to the
file can rewrite the whole chain and the anchor together. See
docs/16_Audit_Log_Design.md.

Confidence: deterministic checks record confidence=null /
confidence_kind=not_probabilistic (docs/04_Rulebook.md).
"""
import contextlib
import hashlib
import hmac
import json
import os
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

from advisory import advisory_checks
from audit import digest, verify

if os.name == 'nt':
    import msvcrt
else:
    import fcntl

GENESIS = '0' * 64

# Every decision the system itself may take. None of them approves, denies or
# submits a claim (docs/01 scope boundaries): the only system decisions are
# routing decisions.
SYSTEM_DECISIONS = {'route_to_human_review', 'no_findings_for_review', 'quarantine_claim'}
AI_ACTION_TYPES = {'human_escalation'}
FORBIDDEN_AI_ACTIONS = {'auto_correct'}

_REQUIRED = {
    'ingestion': {'claim_id', 'source_format', 'outcome'},
    'run_started': {'run_id', 'claim_id', 'input_hash'},
    'rule_check': {'run_id', 'claim_id', 'rule_id', 'rule_version', 'status', 'severity',
                   'result_hash', 'confidence', 'confidence_kind', 'method'},
    'ai_request': {'run_id', 'claim_id', 'rule_id', 'request_id', 'question', 'deterministic_status',
                   'verdict', 'requires_human_review', 'finding_hash', 'prompt_version', 'prompt_hash',
                   'action_type'},
    'ai_recommendation': {'run_id', 'claim_id', 'rule_id', 'request_id', 'model', 'prompt_version',
                          'used_fallback', 'source', 'output_hash', 'action_type', 'auto_correct_applied',
                          'escalated_to', 'confidence', 'confidence_kind'},
    'ai_failure': {'run_id', 'claim_id', 'rule_id', 'request_id', 'error', 'action_type'},
    'system_decision': {'run_id', 'claim_id', 'decision', 'reason'},
    'run_finished': {'run_id', 'claim_id', 'rule_pack_hash', 'tool_errors'},
    'recheck_run': {'claim_id', 'prior_run_id', 'new_run_id', 'prior_input_hash', 'new_input_hash'},
    # a claim_id that was already reviewed, submitted again outside the recheck flow
    'duplicate_submission': {'run_id', 'claim_id', 'prior_run_ids', 'same_input'},
    # a defect none of the 15 rules covers (src/advisory.py); never a rule result
    'advisory_check': {'run_id', 'claim_id', 'check_id', 'detail'},
}

# Events of the reviewer API's security log (src/access/securitylog.py, which also refuses fields that could carry a secret).
# This table only says which fields each type must have.
SECURITY_EVENTS = {
    'login_success': {'badge_id'},
    'login_failure': {'reason'},
    'lockout': {'badge_id'},
    'logout': {'badge_id'},
    'token_rejected': {'reason'},
    'forbidden': {'badge_id', 'method', 'path'},
    'unmask': {'badge_id', 'claim_id', 'reason'},
    'user_created': {'actor', 'badge_id', 'level'},
    'user_updated': {'actor', 'badge_id', 'changes'},
    'user_unlocked': {'actor', 'badge_id'},
    'totp_reset': {'actor', 'badge_id'},
    'password_changed': {'badge_id'},
    'audit_read': {'badge_id'},
    'audit_verify': {'badge_id', 'ok'},
    'decision': {'badge_id', 'claim_id', 'rule_id', 'action'},
    'routing_config_changed': {'actor', 'version', 'before', 'after'},
    'lease_expired': {'claim_id', 'badge_id'},
    'claim_dealt': {'deal_id', 'claim_id', 'badge_id'},
    'claim_decided_green': {'badge_id', 'claim_id', 'action'},
    'triage_receipt': {'claim_id', 'input_hash', 'result_hash', 'lane', 'score', 'config_version'},
}
_REQUIRED.update(SECURITY_EVENTS)


def _validate_system_event(event):
    kind = event.get('event_type')
    if kind not in _REQUIRED:
        raise ValueError(f'Unknown system event type: {kind!r}')
    missing = _REQUIRED[kind] - set(event)
    if missing:
        raise ValueError(f'{kind} event missing fields: {sorted(missing)}')
    if kind == 'system_decision' and event['decision'] not in SYSTEM_DECISIONS:
        raise ValueError(f"System decision {event['decision']!r} is not permitted")
    if kind in ('rule_check', 'ai_recommendation'):
        if event['confidence_kind'] == 'not_probabilistic' and event['confidence'] is not None:
            raise ValueError('not_probabilistic events must have null confidence')
        if event['confidence_kind'] not in ('not_probabilistic', 'uncalibrated', 'calibrated'):
            raise ValueError('Invalid confidence_kind')
    if kind in ('ai_request', 'ai_recommendation', 'ai_failure'):
        if event['action_type'] in FORBIDDEN_AI_ACTIONS:
            raise ValueError('AI auto-correction is not permitted: AI actions must be human_escalation')
        if event['action_type'] not in AI_ACTION_TYPES:
            raise ValueError(f"Unknown AI action type: {event['action_type']!r}")
    if kind == 'ai_recommendation' and event['auto_correct_applied'] is not False:
        raise ValueError('auto_correct_applied must be false: the AI never changes a claim or result')


_THREAD_LOCKS = {}
_THREAD_LOCKS_GUARD = threading.Lock()
LOCK_TIMEOUT_SECONDS = 30


def _thread_lock(path):
    key = str(Path(path).resolve())
    with _THREAD_LOCKS_GUARD:
        return _THREAD_LOCKS.setdefault(key, threading.RLock())


@contextlib.contextmanager
def _file_lock(lock_path, timeout=LOCK_TIMEOUT_SECONDS):
    """Exclusive cross-process lock (msvcrt on Windows, flock elsewhere) on a sidecar file."""
    with open(lock_path, 'a+b') as fh:
        deadline = time.monotonic() + timeout
        while True:
            try:
                fh.seek(0)
                if os.name == 'nt':
                    msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise TimeoutError(f'Could not lock {lock_path} within {timeout}s') from None
                time.sleep(0.005)
        try:
            yield
        finally:
            fh.seek(0)
            if os.name == 'nt':
                msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(fh, fcntl.LOCK_UN)


ANCHOR_KEY_ENV = 'AUDIT_ANCHOR_KEY'





def _anchor_mac(head, count):

    """HMAC-SHA256 over the anchored head and count, keyed by AUDIT_ANCHOR_KEY, or None when no key is configured.

    Without a key, whoever can write the log can also rewrite the chain and the anchor together and still verify;

    with one, forging the anchor needs a secret the log writer's storage does not hold."""

    key = os.environ.get(ANCHOR_KEY_ENV)

    if not key:

        return None

    return hmac.new(key.encode('utf-8'), f'{head}|{count}'.encode('ascii'), hashlib.sha256).hexdigest()





class AuditLog:
    """Append-only chain writer, safe for several threads and processes.

    Every append takes a thread lock and an OS file lock, then RE-READS the log's real last row
    to learn the current head and count (the values cached at open may be stale because another
    writer appended). Without that, two writers fork the chain. The anchor is replaced atomically
    (temp file + os.replace), and records that must survive a crash (the write-ahead ai_request,
    human decisions) are fsync'd."""

    DURABLE_EVENTS = {'ai_request'}

    def __init__(self, path):
        self.path = Path(path)
        self.anchor_path = self.path.with_name(self.path.name + '.head.json')
        self.lock_path = self.path.with_name(self.path.name + '.lock')
        self.path.parent.mkdir(parents=True, exist_ok=True)
        # If an anchor exists, check against it BEFORE anything is appended. Otherwise the
        # next write would re-anchor the truncated state and erase the evidence.
        # Under the same locks _write_anchor() writes under: an unlocked read here can catch
        # another writer mid os.replace() (a transient PermissionError, retried by
        # _read_anchor_text) or mid-append (a false "truncated" reading against a since-moved
        # anchor, which retrying alone cannot fix -- this needs to not race the writer at all).
        with _thread_lock(self.path), _file_lock(self.lock_path):
            if self.anchor_path.exists():
                self.head, self.count = verify_with_anchor(self.path, self.anchor_path)
            else:
                self.head, self.count = verify(self.path)

    def _sync_index(self):

        """Incrementally read what has been appended since the last call: which runs each claim_id already has, and

        which run ids came from the recheck flow. Called under the lock. The log is append-only, so an offset is enough."""

        if not hasattr(self, '_idx_offset'):

            self._idx_offset, self._runs, self._recheck_ids = 0, {}, set()

        if not self.path.exists():

            return

        with open(self.path, 'rb') as f:

            f.seek(self._idx_offset)

            data = f.read()

        end = data.rfind(bytes([10]))

        if end < 0:

            return

        self._idx_offset += end + 1

        for raw in data[:end].split(bytes([10])):

            if not raw.strip():

                continue

            event = json.loads(raw.decode('utf-8'))['event']

            if event.get('event_type') == 'run_started':

                self._runs.setdefault(event['claim_id'], []).append((event['run_id'], event['input_hash']))

            elif event.get('event_type') == 'recheck_run':

                self._recheck_ids.add(event['new_run_id'])



    def append_run_start(self, events, claim_id, run_id, input_hash):

        """Write a run's opening events. If this claim_id was already reviewed and this run is not a reviewer-requested

        recheck, a duplicate_submission event is written in the same locked append, so two concurrent submissions of

        one claim cannot both miss each other. Returns the duplicate details, or None."""

        for e in events:

            _validate_system_event(e)

        with self._locked():

            self._sync_index()

            prior = self._runs.get(claim_id, [])

            duplicate = None

            if prior and run_id not in self._recheck_ids:

                duplicate = {'prior_run_ids': [r for r, _ in prior], 'same_input': any(h == input_hash for _, h in prior)}

                event = {'event_type': 'duplicate_submission', 'run_id': run_id, 'claim_id': claim_id, **duplicate}

                _validate_system_event(event)

                events = events + [event]

            self._write(events, durable=False)

            return duplicate



    @contextlib.contextmanager
    def _locked(self):
        with _thread_lock(self.path), _file_lock(self.lock_path):
            self._sync_tail()
            yield

    def _sync_tail(self):
        """Adopt the log's actual last row as (head, count). Refuses a torn or altered tail."""
        if not self.path.exists() or self.path.stat().st_size == 0:
            self.head, self.count = GENESIS, 0
            return
        size = self.path.stat().st_size
        chunk = 1 << 16
        with open(self.path, 'rb') as f:
            while True:
                start = max(0, size - chunk)
                f.seek(start)
                data = f.read(size - start)
                body = data.rstrip(b'\n')
                if b'\n' in body or start == 0:
                    last = body.rsplit(b'\n', 1)[-1]
                    break
                chunk *= 2
        try:
            row = json.loads(last.decode('utf-8'))
            claimed = row.pop('hash')
            ok = digest(row) == claimed
        except (ValueError, KeyError, UnicodeDecodeError):
            ok = False
        if not ok:
            raise ValueError('Audit log tail is torn or altered; refusing to append')
        self.head, self.count = claimed, row['sequence']

    def append_system_events(self, events):
        for e in events:
            _validate_system_event(e)
        if not events:
            return self.head, self.count
        durable = any(e['event_type'] in self.DURABLE_EVENTS for e in events)
        with self._locked():
            return self._write(events, durable)

    def append_review_decisions(self, events):
        """Human decisions: delegate validation to the supplied audit.append()
        rules, then re-sync tail state. Kept strict on purpose."""
        from audit import append
        with self._locked():
            self.head, self.count = append(self.path, events)
            self._fsync_file()
            self._write_anchor()
            return self.head, self.count

    def _fsync_file(self):
        with open(self.path, 'ab') as f:  # a writable handle: fsync on a read-only one fails on Windows
            f.flush()
            os.fsync(f.fileno())

    def _write(self, events, durable=False):
        with self.path.open('a', encoding='utf-8') as f:
            for event in events:
                row = {'sequence': self.count + 1,
                       'recorded_at': datetime.now(timezone.utc).isoformat(),
                       'previous_hash': self.head, 'event': event}
                self.head = digest(row)
                # ASCII-escaped so a U+2028 inside a claim value can never split a log line for a splitlines()
                # reader. The hash is over the parsed row (digest), so how the row is spelled on disk is irrelevant.
                f.write(json.dumps({**row, 'hash': self.head}) + '\n')
                self.count += 1
            f.flush()
            if durable:
                os.fsync(f.fileno())
        self._write_anchor()
        return self.head, self.count

    def _write_anchor(self):
        tmp = self.anchor_path.with_name(self.anchor_path.name + f'.{os.getpid()}.tmp')
        anchor = {
            'head': self.head, 'count': self.count,
            'written_at': datetime.now(timezone.utc).isoformat(),
            'note': 'Keep a copy of this file somewhere the log writer cannot modify.',
        }
        mac = _anchor_mac(self.head, self.count)
        if mac:
            anchor['mac'] = mac
        tmp.write_text(json.dumps(anchor, indent=2), encoding='utf-8')
        # Windows refuses to replace a file another process has open for an instant (a reader, a
        # verifier, a scanner), even though the lock serialises writers. Retry briefly; on POSIX
        # the first attempt always succeeds.
        for attempt in range(40):
            try:
                os.replace(tmp, self.anchor_path)
                return
            except PermissionError:
                if attempt == 39:
                    tmp.unlink(missing_ok=True)
                    raise
                time.sleep(0.005 * (attempt + 1))


def _read_anchor_text(anchor_path):
    """Read the anchor file, retrying on Windows sharing-violation PermissionErrors: a reader can
    land mid another writer's os.replace() (see _write_anchor's matching retry on the write side).
    Same backoff budget as that side (40 attempts, ~4.1s total) for symmetry."""
    for attempt in range(40):
        try:
            return anchor_path.read_text(encoding='utf-8')
        except PermissionError:
            if attempt == 39:
                raise
            time.sleep(0.005 * (attempt + 1))


def verify_with_anchor(log_path, anchor_path=None, strict=False):
    """Full chain verification plus comparison against the anchor. Returns
    (head, count). Raises ValueError on a broken chain, truncation, or a
    replaced log.

    strict=True also rejects rows AFTER the anchored position. A writer rewrites the anchor after every
    append, so on a quiet log count always equals the anchor's count; rows beyond it are either a crash
    between the log write and the anchor write or rows an attacker appended with a valid chain. It is off
    by default because a reader that does not hold the lock can legitimately catch a writer mid-append
    (AuditLog.__init__ and other live callers); offline verification of a finished log should use strict."""
    log_path = Path(log_path)
    anchor_path = Path(anchor_path) if anchor_path else log_path.with_name(log_path.name + '.head.json')
    head, count = verify(log_path)
    if not anchor_path.exists():
        raise ValueError('No anchor file: chain is internally consistent but cannot be checked against truncation')
    try:
        anchor = json.loads(_read_anchor_text(anchor_path))
        anchor_count, anchor_head = int(anchor['count']), anchor['head']
    except (ValueError, KeyError, TypeError, PermissionError) as e:
        raise ValueError(f'Anchor file {anchor_path} is unreadable or malformed: {e}') from e
    if _anchor_mac(anchor_head, anchor_count) is not None:  # a key is configured: the anchor must be signed with it
        if not hmac.compare_digest(str(anchor.get('mac', '')), _anchor_mac(anchor_head, anchor_count)):
            raise ValueError('Anchor MAC missing or wrong: the anchor was not written with the configured key')
    if count < anchor_count:
        raise ValueError(f'Log truncated: {count} events, anchor recorded {anchor_count}')
    if strict and count > anchor_count:
        raise ValueError(f'{count - anchor_count} unanchored row(s): the log has {count} events but the anchor recorded {anchor_count}')
    if anchor_count == 0:
        return head, count
    rows = [json.loads(l) for l in log_path.read_text(encoding='utf-8').split('\n') if l.strip()]
    if rows[anchor_count - 1]['hash'] != anchor_head:
        raise ValueError('Log replaced: hash at anchored position differs from the anchor')
    return head, count


def anchor_status(log_path, anchor_path=None):
    """Whether the anchor next to a log is HMAC-signed, and whether a key is configured in this process. Reports only;
    never raises, and never replaces verify_with_anchor (which is what actually checks the signature)."""
    log_path = Path(log_path)
    anchor_path = Path(anchor_path) if anchor_path else log_path.with_name(log_path.name + '.head.json')
    try:
        signed = bool(json.loads(_read_anchor_text(anchor_path)).get('mac'))
    except (OSError, ValueError, TypeError, AttributeError):
        signed = False
    return {'key_configured': _anchor_mac('', 0) is not None, 'anchor_signed': signed}


class AuditHooks:
    """review_package() lifecycle hooks that write the audit trail AHEAD of each action."""

    def __init__(self, log, source_format='normalized_json', ingestion_report=None):
        self.log = log
        self.source_format = source_format
        self.report = ingestion_report or {}
        self.duplicate = None

    def on_start(self, run_id, claim, input_hash, started_at):
        self.duplicate = self.log.append_run_start([
            {'event_type': 'ingestion', 'claim_id': claim['claim_id'], 'source_format': self.source_format,
             'outcome': 'accepted', 'warnings': self.report.get('warnings', []),
             'not_carried_by_source': self.report.get('not_carried_by_fhir', [])},
            {'event_type': 'run_started', 'run_id': run_id, 'claim_id': claim['claim_id'],
             'input_hash': input_hash, 'started_at': started_at},
        ], claim['claim_id'], run_id, input_hash)

    def on_checks(self, run_id, claim, rule_results):
        self.log.append_system_events([{
            'event_type': 'rule_check', 'run_id': run_id, 'claim_id': claim['claim_id'],
            'rule_id': r['rule_id'], 'rule_version': r['rule_version'], 'status': r['status'],
            'severity': r['severity'], 'affected_line_ids': r['affected_line_ids'],
            'result_hash': digest(r), 'confidence': r['confidence'],
            'confidence_kind': r['confidence_kind'], 'method': r['method'],
        } for r in rule_results])

    def before_ai(self, request):
        self.log.append_system_events([{'event_type': 'ai_request', **request}])

    def after_ai(self, request, drafted, error):
        base = {'run_id': request['run_id'], 'claim_id': request['claim_id'], 'rule_id': request['rule_id'],
                'request_id': request['request_id'], 'action_type': request['action_type']}
        if drafted is None:
            self.log.append_system_events([{'event_type': 'ai_failure', **base, 'error': error}])
            return
        used_fallback = drafted['used_fallback']
        # The text came from a template if the fallback ran OR the primary provider is itself the template.
        from_template = used_fallback or request['model'] == 'deterministic-template'
        self.log.append_system_events([{
            'event_type': 'ai_recommendation', **base,
            # a cascade names the tier that actually answered; a single provider is its own model
            'model': 'deterministic-template' if from_template else (drafted.get('answered_by') or request['model']),
            'tier_errors': drafted.get('tier_errors', []),
            'citation_repairs': drafted.get('citation_repairs', []),
            'prompt_version': request['prompt_version'], 'used_fallback': used_fallback,
            'source': 'deterministic_template' if from_template else 'model',
            'error': drafted['error'], 'latency_ms': drafted['latency_ms'], 'usage': drafted['usage'],
            'attempts': drafted.get('attempts'),
            'engine_explanation': drafted.get('engine_explanation'),
            'omitted_engine_reasons': drafted.get('omitted_engine_reasons', []),
            'output_hash': digest(drafted['output']), 'explanation': drafted['output']['explanation'],
            'auto_correct_applied': False, 'escalated_to': 'human_reviewer',
            # The explanation contract carries no score; never fabricate one.
            'confidence': None, 'confidence_kind': 'not_probabilistic',
        }])


def audited_review(log, claim, cfg, provider=None, fallback=None, untrusted_note=None,
                   source_format='normalized_json', ingestion_report=None, run_id=None):
    """Run review_package() with write-ahead auditing. Returns the same
    (rule_results, ai_explanations, run_trace)."""
    from claim_review import review_package
    hooks = AuditHooks(log, source_format, ingestion_report)
    rule_results, ai, trace = review_package(claim, cfg, provider=provider, fallback=fallback,
                                             untrusted_note=untrusted_note, hooks=hooks, run_id=run_id)
    claim_id, run_id = trace['claim_id'], trace['run_id']
    if rule_results is None:
        log.append_system_events([
            {'event_type': 'ingestion', 'claim_id': claim_id, 'source_format': source_format,
             'outcome': 'quarantined', 'error': trace.get('ingestion_error')},
            {'event_type': 'system_decision', 'run_id': run_id, 'claim_id': claim_id,
             'decision': 'quarantine_claim', 'reason': trace.get('ingestion_error', 'ingestion failed')},
        ])
        return rule_results, ai, trace
    needs_review = [r for r in rule_results if r['requires_human_review']]
    # Beyond the 15 rules: defects no rule covers. Recorded, never scored, and they only ever add a reason to look.
    advisories = advisory_checks(claim, cfg)
    trace['advisories'] = advisories
    if advisories:
        log.append_system_events([{'event_type': 'advisory_check', 'run_id': run_id, 'claim_id': claim_id, **a}
                                  for a in advisories])
    duplicate = hooks.duplicate
    reasons = []
    if needs_review:
        reasons.append(f"{len(needs_review)} finding(s) require human review: "
                       + ', '.join(f"{r['rule_id']}={r['status']}" for r in needs_review))
    if duplicate:
        reasons.append(f"duplicate submission: claim_id already reviewed in {len(duplicate['prior_run_ids'])} earlier run(s), "
                       + ('identical content' if duplicate['same_input'] else 'CHANGED content'))
    if advisories:
        reasons.append('advisory checks outside the 15 rules: ' + ', '.join(a['check_id'] for a in advisories))
    log.append_system_events([
        {'event_type': 'system_decision', 'run_id': run_id, 'claim_id': claim_id,
         'decision': 'route_to_human_review' if reasons else 'no_findings_for_review',
         'reason': '; '.join(reasons) if reasons
                   else 'No FAIL or UNABLE_TO_ASSESS findings. This is not an approval.'},
        {'event_type': 'run_finished', 'run_id': run_id, 'claim_id': claim_id,
         'rule_pack_hash': trace['rule_pack_hash'], 'engine_code_hash': trace['engine_code_hash'],
         'rule_versions': trace['rule_versions'],
         'tool_errors': trace['tool_errors'], 'finished_at': trace['finished_at']},
    ])
    return rule_results, ai, trace


def audited_review_many(log, items, cfg, provider=None, fallback=None, workers=8, on_done=None):
    """Review many claims concurrently, each with write-ahead auditing.

    `items` is a list of dicts: {'claim', optional 'source_format', 'ingestion_report',
    'untrusted_note'}. Returns [(rule_results, ai, trace)] in INPUT order. Claims are independent,
    so each runs its own bounded sequence in a worker thread; every audit write is serialized by the
    log's lock and each ai_request is still written before its own model call. Events of different
    claims interleave in the log, which verify_ai_ordering() tolerates because it keys on run/request
    ids. A claim whose worker raises (for example a log write failure) re-raises here after the other
    workers finish, so a failure is never silent."""
    from concurrent.futures import ThreadPoolExecutor

    def one(item):
        out = audited_review(log, item['claim'], cfg, provider=provider, fallback=fallback,
                             untrusted_note=item.get('untrusted_note'),
                             source_format=item.get('source_format', 'normalized_json'),
                             ingestion_report=item.get('ingestion_report'))
        if on_done:
            on_done(item, out)
        return out

    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futures = [pool.submit(one, it) for it in items]
        return [f.result() for f in futures]


def verify_ai_ordering(log_path):
    """Check, from the log alone, that every AI action was registered before it happened and
    classified. Raises ValueError listing violations; returns summary statistics.

    For every ai_request: the run had started and the rule's deterministic rule_check (with the
    same status) was already logged; the action type is human_escalation. For every outcome
    (ai_recommendation / ai_failure): a matching earlier ai_request exists, answered once, same
    run / claim / rule, recorded no earlier than the request, action type human_escalation and
    auto_correct_applied false. A run that finished with an unanswered request is a violation."""
    rows = [json.loads(l) for l in Path(log_path).read_text(encoding='utf-8').split('\n') if l.strip()]
    started, checks, finished = set(), {}, set()
    requests, answered = {}, set()
    problems = []
    stats = {'ai_requests': 0, 'ai_recommendations': 0, 'ai_failures': 0, 'unanswered_requests': 0,
             'action_types': {}, 'sources': {}}
    for row in rows:
        e, seq = row['event'], row['sequence']
        kind = e.get('event_type')
        if kind == 'run_started':
            started.add(e['run_id'])
        elif kind == 'run_finished':
            finished.add(e['run_id'])
        elif kind == 'rule_check':
            checks[(e['run_id'], e['rule_id'])] = e['status']
        elif kind == 'ai_request':
            stats['ai_requests'] += 1
            rid = e['request_id']
            if rid in requests:
                problems.append(f'seq {seq}: duplicate request_id {rid}')
            if e['run_id'] not in started:
                problems.append(f"seq {seq}: ai_request before run_started for run {e['run_id']}")
            logged = checks.get((e['run_id'], e['rule_id']))
            if logged is None:
                problems.append(f"seq {seq}: ai_request for {e['rule_id']} before its rule_check was logged")
            elif logged != e['deterministic_status']:
                problems.append(f"seq {seq}: ai_request status {e['deterministic_status']} != logged rule_check {logged}")
            if e['action_type'] not in AI_ACTION_TYPES:
                problems.append(f"seq {seq}: forbidden/unknown action_type {e['action_type']!r}")
            requests[rid] = (seq, row['recorded_at'], e)
        elif kind in ('ai_recommendation', 'ai_failure'):
            stats['ai_recommendations' if kind == 'ai_recommendation' else 'ai_failures'] += 1
            rid = e.get('request_id')
            req = requests.get(rid)  # nosec B113 (a local dict, not the HTTP library)
            if req is None:
                problems.append(f'seq {seq}: {kind} with no earlier ai_request ({rid})')
                continue
            if rid in answered:
                problems.append(f'seq {seq}: request {rid} answered twice')
            answered.add(rid)
            rq = req[2]
            if (e['run_id'], e['claim_id'], e['rule_id']) != (rq['run_id'], rq['claim_id'], rq['rule_id']):
                problems.append(f'seq {seq}: {kind} does not match its request')
            if row['recorded_at'] < req[1]:
                problems.append(f'seq {seq}: {kind} recorded before its request')
            if e['action_type'] not in AI_ACTION_TYPES:
                problems.append(f"seq {seq}: forbidden/unknown action_type {e['action_type']!r}")
            stats['action_types'][e['action_type']] = stats['action_types'].get(e['action_type'], 0) + 1
            if kind == 'ai_recommendation':
                if e['auto_correct_applied'] is not False:
                    problems.append(f'seq {seq}: auto_correct_applied is not false')
                stats['sources'][e['source']] = stats['sources'].get(e['source'], 0) + 1
    for rid, (seq, _, rq) in requests.items():
        if rid not in answered:
            if rq['run_id'] in finished:
                problems.append(f'seq {seq}: request {rid} was never answered although its run finished')
            else:
                stats['unanswered_requests'] += 1
    if problems:
        raise ValueError('AI audit ordering violations:\n  ' + '\n  '.join(problems[:20]))
    return stats


def events_for_quarantined_record(source_ref, source_format, error, claim_id='UNKNOWN'):
    """A record that never became a claim (bad JSON, unmappable bundle, failed
    transport contract). It is logged and routed to quarantine, never dropped."""
    return [
        {'event_type': 'ingestion', 'claim_id': claim_id, 'source_format': source_format,
         'outcome': 'quarantined', 'source_ref': source_ref, 'error': error},
        {'event_type': 'system_decision', 'run_id': None, 'claim_id': claim_id,
         'decision': 'quarantine_claim', 'reason': error},
    ]
