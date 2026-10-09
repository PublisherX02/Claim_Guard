"""Cross-check of the audit log: a log written by one build must verify under the other, including its anchor, strictly.

    python rust/tools/compare_audit.py [--cg rust/target/release/cg.exe]

  1. Python writes a log (many event shapes: floats, ints, unicode, line separators, nested values); Rust verifies it, strictly.
  2. Rust writes a log from the same events; Python verifies it, strictly, with and without an anchor key.
  3. Tampering (an edited row, a truncated log, a forged appended row) is caught by BOTH verifiers, on a log from EITHER build.
  4. The two builds compute the same head hash for the same chain content (same rows in the same order give the same digest rule).
Exit code 0 only when everything agrees.
"""
import argparse, json, os, shutil, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
import audit_log  # noqa: E402
from audit import digest  # noqa: E402

EVENTS = [
    {'event_type': 'login_success', 'badge_id': 'CG-2002', 'client': '127.0.0.1'},
    {'event_type': 'login_failure', 'reason': 'bad_password', 'note': 'café   line sep \u0085 nel \U0001f600 emoji'},
    {'event_type': 'decision', 'badge_id': 'CG-2002', 'claim_id': 'CG-1', 'rule_id': 'R001', 'action': 'confirm_issue', 'at': 1791554317.0560255},
    {'event_type': 'triage_receipt', 'claim_id': 'CG-1', 'input_hash': 'a' * 64, 'result_hash': 'b' * 64, 'lane': 'A', 'score': 4, 'config_version': 1,
     'floats': [0.1, 2.0, 1e16, 1e-05, 123456789.123456789, -0.0, 5e-324], 'big': 123456789012345678901234567890, 'nested': {'z': [None, True, False], 'a': {}}},
    {'event_type': 'routing_config_changed', 'actor': 'CG-4004', 'version': 2, 'before': {'slice_size': 6}, 'after': {'slice_size': 8}},
    {'event_type': 'forbidden', 'badge_id': 'CG-1001', 'method': 'GET', 'path': '/api/v1/queue/dashboard'},
    {'event_type': 'logout', 'badge_id': 'quote " back \\ slash \t tab \x1f ctl'},
]
REVIEW = [{'action': 'dismiss_with_reason', 'actor': 'CG-2002', 'claim_id': 'CG-1', 'rule_id': 'R001', 'reason': 'checked the source – fine'}]


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, encoding='utf-8')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cg', default=str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
    a = ap.parse_args()
    failures = []

    def check(name, ok, detail=''):
        print(('ok   ' if ok else 'FAIL ') + name + (f'  {detail}' if detail and not ok else ''))
        if not ok:
            failures.append(name)

    def py_verify(path, strict=True):
        try:
            return True, audit_log.verify_with_anchor(path, strict=strict)
        except Exception as e:  # noqa: BLE001
            return False, str(e)

    def rs_verify(path, strict=True):
        r = run([a.cg, 'audit-verify', str(path)] + (['--strict'] if strict else []))
        return r.returncode == 0, (r.stdout + r.stderr).strip()

    for key in (None, 'k' * 40):
        label = 'with anchor key' if key else 'without anchor key'
        if key:
            os.environ[audit_log.ANCHOR_KEY_ENV] = key
        else:
            os.environ.pop(audit_log.ANCHOR_KEY_ENV, None)
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            events_file = tmp / 'events.jsonl'
            events_file.write_text('\n'.join(json.dumps(e) for e in EVENTS) + '\n', encoding='utf-8')
            review_file = tmp / 'review.jsonl'
            review_file.write_text('\n'.join(json.dumps(e) for e in REVIEW) + '\n', encoding='utf-8')

            # 1. Python writes, Rust verifies
            py_log = tmp / 'py.jsonl'
            log = audit_log.AuditLog(py_log)
            log.append_system_events(EVENTS)
            log.append_review_decisions(REVIEW)
            ok_rs, msg = rs_verify(py_log)
            ok_py, py_msg = py_verify(py_log)
            check(f'[{label}] Rust verifies a log written by Python', ok_rs, msg)
            check(f'[{label}] both builds report the same head and count', ok_rs and ok_py and msg.split('head ')[-1].rstrip('.') == py_msg[0] and f'{py_msg[1]} events' in msg, f'{msg} / {py_msg}')

            # 2. Rust writes, Python verifies
            rs_log = tmp / 'rs.jsonl'
            r1 = run([a.cg, 'audit-append', str(rs_log), str(events_file)])
            r2 = run([a.cg, 'audit-append', str(rs_log), str(review_file), '--review'])
            check(f'[{label}] Rust writes the same events and decisions', r1.returncode == 0 and r2.returncode == 0, r1.stderr + r2.stderr)
            ok_py, py_msg = py_verify(rs_log)
            check(f'[{label}] Python verifies a log written by Rust (strict, with anchor)', ok_py, str(py_msg))
            # Python can keep appending to a Rust log, and Rust to a Python log
            audit_log.AuditLog(rs_log).append_system_events([EVENTS[0]])
            run([a.cg, 'audit-append', str(py_log), str(events_file)])
            check(f'[{label}] each build can append to the other build\'s log', py_verify(rs_log)[0] and rs_verify(rs_log)[0] and py_verify(py_log)[0] and rs_verify(py_log)[0])

            # 3. tampering is caught by both verifiers, on either log
            for name, log_path in (('python-written', py_log), ('rust-written', rs_log)):
                work = tmp / f'tamper-{name}.jsonl'
                for suffix in ('', '.head.json'):
                    shutil.copy(str(log_path) + suffix, str(work) + suffix)
                lines = work.read_text(encoding='utf-8').split('\n')
                edited = work.with_name('edited.jsonl')
                for suffix in ('.head.json',):
                    shutil.copy(str(work) + suffix, str(edited) + suffix)
                edited.write_text('\n'.join([lines[0].replace('CG-2002', 'CG-9999')] + lines[1:]), encoding='utf-8')
                check(f'[{label}] an edited row on a {name} log is caught by both', not py_verify(edited)[0] and not rs_verify(edited)[0])
                truncated = work.with_name('truncated.jsonl')
                shutil.copy(str(work) + '.head.json', str(truncated) + '.head.json')
                truncated.write_text('\n'.join(lines[:-3]) + '\n', encoding='utf-8')
                check(f'[{label}] a truncated {name} log is caught by both', not py_verify(truncated)[0] and not rs_verify(truncated)[0])
                forged = work.with_name('forged.jsonl')
                shutil.copy(str(work) + '.head.json', str(forged) + '.head.json')
                last = json.loads([l for l in lines if l.strip()][-1])
                row = {'sequence': last['sequence'] + 1, 'recorded_at': 'x', 'previous_hash': last['hash'], 'event': EVENTS[0]}
                row['hash'] = digest(row)
                forged.write_text('\n'.join(l for l in lines if l.strip()) + '\n' + json.dumps(row) + '\n', encoding='utf-8')
                check(f'[{label}] a forged row appended to a {name} log is caught (strict) by both', not py_verify(forged)[0] and not rs_verify(forged)[0])
                if key:
                    wrong = work.with_name('wrongkey.jsonl')
                    shutil.copy(str(work), str(wrong))
                    anchor = json.loads(Path(str(work) + '.head.json').read_text())
                    anchor['mac'] = '0' * 64
                    Path(str(wrong) + '.head.json').write_text(json.dumps(anchor))
                    check(f'[{label}] a wrong anchor signature on a {name} log is caught by both', not py_verify(wrong)[0] and not rs_verify(wrong)[0])
    print('ALL AGREE' if not failures else f'{len(failures)} checks failed')
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
