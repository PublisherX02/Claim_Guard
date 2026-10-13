"""Score the document-to-claim pipeline (gate -> reader sidecar -> extraction) on synthetic documents with a known answer.

    python rust/tools/make_docs.py --out <dir> --n 30
    python rust/tools/doc_eval.py --docs <dir> --engines tesseract,both [--limit N] [--out report.json]

For each engine configuration of the reader sidecar it starts the sidecar, runs `cg ingest-doc` over every document and compares each field of the draft
claim with the truth. What matters most is not the accuracy alone but the SILENT errors: a field that is wrong and that the draft did not flag (not empty, no
warning about it, no note that the readers disagree or that the engine was unsure). A flagged error costs a person a glance; a silent one reaches the rules.
Reported per condition (digital PDF text layer, scan, photo, image-only PDF) and layout: field accuracy, documents with every field right, wrong fields that
were flagged, silent errors, and the time per document.
Environment for the sidecar: TESSERACT_CMD, TESSDATA_PREFIX; for the vision reader VLM_BASE_URL and VLM_MODEL (default: local Ollama with richardyoung/unlimited-ocr).
"""
import argparse
import collections
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CG = os.environ.get('CG_BIN', str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'))
PORT = 8092
TOKEN = 'doc-eval-token-' + str(os.getpid())
SCALARS = ['claim_id', 'invoice_number', 'patient_id', 'member_id', 'provider_id', 'payer_id', 'policy_id', 'diagnosis_code', 'submission_date', 'currency', 'total_amount']
LINE_KEYS = ['line_id', 'service_code', 'service_date', 'modifier', 'quantity', 'unit_price', 'net_amount', 'authorization_id']
NOTE_FLAGS = ('disagree', 'NOT confirmed', 'unsure', 'ambiguous', 'not a real date')


def same(a, b):
    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and not isinstance(a, bool) and not isinstance(b, bool):
        return abs(float(a) - float(b)) < 1e-9
    return a == b


def paths_of(truth, draft_claim):
    """[(path, truth_value, draft_value)] for every field the page shows; a line the draft does not have counts as missing."""
    out = [(k, truth[k], draft_claim.get(k)) for k in SCALARS]
    cov = draft_claim.get('coverage') or {}
    for k in ('status', 'start_date', 'end_date'):
        out.append(('coverage.' + k, truth['coverage'][k], cov.get(k)))
    lines = draft_claim.get('lines') or []
    for i, tl in enumerate(truth['lines']):
        dl = lines[i] if i < len(lines) else {}
        for k in LINE_KEYS:
            out.append(('lines[%d].%s' % (i, k), tl[k], dl.get(k)))
    return out


def flagged(path, value, draft):
    if value is None:
        return True
    warns = draft.get('warnings') or []
    if any(w.startswith(path) for w in warns):
        return True
    if path.startswith('lines['):
        prefix = path.split('.')[0] + ':'
        if any(w.startswith(prefix) for w in warns):
            return True
        if path.endswith(('net_amount', 'quantity', 'unit_price')) and any(w.startswith('the lines add up') for w in warns):
            return True
    if path in ('total_amount',) and any(w.startswith('the lines add up') for w in warns):
        return True
    for f in draft.get('fields') or []:
        if f['path'] == path and (f.get('low_confidence') or any(any(flag in n for flag in NOTE_FLAGS) for n in f.get('notes') or [])):
            return True
    return False


def start_sidecar(engine, env_extra):
    env = dict(os.environ, OCR_TOKEN=TOKEN, OCR_PORT=str(PORT), OCR_HOST='127.0.0.1', OCR_ENGINE=engine, **env_extra)
    p = subprocess.Popen([sys.executable, str(ROOT / 'deploy' / 'ocr' / 'service.py')], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(40):
        try:
            req = urllib.request.Request('http://127.0.0.1:%d/health' % PORT, headers={'Authorization': 'Bearer ' + TOKEN})
            urllib.request.urlopen(req, timeout=2)
            return p
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    p.kill()
    raise SystemExit('the reader did not start')


def run_engine(engine, docs, files, env_extra, tmp):
    p = start_sidecar(engine, env_extra)
    try:
        env = dict(os.environ, DOCUMENT_READER_URL='http://127.0.0.1:%d' % PORT, DOCUMENT_READER_TOKEN=TOKEN, DOCUMENT_READER_ENGINE=engine, CLAIMGUARD_ROOT=str(ROOT))
        rows, t0 = [], time.time()
        for start in range(0, len(files), 40):          # a Windows command line is limited to about 32,000 characters
            chunk = files[start:start + 40]
            out = tmp / ('drafts_%s_%d.jsonl' % (engine, start))
            r = subprocess.run([CG, 'ingest-doc', *[str(docs / f) for f in chunk], '--output', str(out)], env=env, capture_output=True, text=True, encoding='utf-8')
            if r.returncode != 0:
                raise SystemExit('cg ingest-doc failed: ' + r.stderr[-300:])
            rows += [json.loads(l) for l in out.read_text(encoding='utf-8').split(chr(10)) if l.strip()]
        return rows, time.time() - t0
    finally:
        p.kill()


def score(rows, truth):
    stats = collections.defaultdict(collections.Counter)
    errors = []
    for row in rows:
        name = row['file']
        cond = name.rsplit('_', 1)[1].split('.')[0]
        lay = 'layout ' + name.split('_')[-2]
        keys = (('condition', cond), ('layout', lay), ('all', 'all'))
        draft = row.get('draft') or {}
        claim = draft.get('claim')
        if row.get('refused') or row.get('error') or not claim:
            for k in keys:
                stats[k]['docs'] += 1
                stats[k]['no_claim'] += 1
            continue
        t = truth[name]
        wrong_any = silent_any = False
        for path, tv, dv in paths_of(t, claim):
            ok = same(tv, dv)
            fl = flagged(path, dv, draft)
            for k in keys:
                s = stats[k]
                s['fields'] += 1
                s['right'] += ok
                if not ok:
                    s['wrong'] += 1
                    s['wrong_flagged'] += fl
                    s['silent'] += (not fl)
            if not ok:
                wrong_any = True
                if not fl:
                    silent_any = True
                    errors.append((name, path, tv, dv))
        for k in keys:
            stats[k]['docs'] += 1
            stats[k]['docs_all_right'] += (not wrong_any)
            stats[k]['docs_silent'] += silent_any
            stats[k]['docs_valid'] += bool(draft.get('valid'))
            stats[k]['docs_clean'] += (not wrong_any) and not draft.get('warnings')
    return stats, errors


def show(engine, stats, dt, n):
    print(chr(10) + '== reader: %s   (%d documents, %.1f s per document through the whole pipeline)' % (engine, n, dt / max(1, n)))
    print('%-18s %5s %9s %11s %6s %14s %7s %12s' % ('', 'docs', 'fields ok', 'docs all ok', 'wrong', 'wrong flagged', 'silent', 'docs silent'))
    for group in ('condition', 'layout', 'all'):
        for k in sorted(k for k in stats if k[0] == group):
            s = stats[k]
            print('%-18s %5d %8.1f%% %10.1f%% %6d %14d %7d %12d' % (k[1], s['docs'], 100 * s['right'] / max(1, s['fields']), 100 * s['docs_all_right'] / max(1, s['docs']), s['wrong'], s['wrong_flagged'], s['silent'], s['docs_silent']))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--docs', required=True)
    ap.add_argument('--engines', default='tesseract,both')
    ap.add_argument('--limit', type=int, default=0, help='use only the first N documents of each condition and layout (0 = all)')
    ap.add_argument('--out')
    a = ap.parse_args()
    docs = Path(a.docs)
    truth = json.loads((docs / 'truth.json').read_text(encoding='utf-8'))
    files = sorted(truth)
    if a.limit:
        per = collections.Counter()
        keep = []
        for f in files:
            k = f.split('_', 1)[1]
            per[k] += 1
            if per[k] <= a.limit:
                keep.append(f)
        files = keep
    env_extra = {k: os.environ[k] for k in ('TESSERACT_CMD', 'TESSDATA_PREFIX') if k in os.environ}
    env_extra.setdefault('VLM_BASE_URL', os.environ.get('VLM_BASE_URL', 'http://127.0.0.1:11434/v1'))
    env_extra.setdefault('VLM_MODEL', os.environ.get('VLM_MODEL', 'richardyoung/unlimited-ocr'))
    tmp = Path(os.environ.get('TEMP', '/tmp')) / 'cg_doc_eval'
    tmp.mkdir(parents=True, exist_ok=True)
    report = {}
    for engine in a.engines.split(','):
        rows, dt = run_engine(engine, docs, files, env_extra, tmp)
        stats, errors = score(rows, truth)
        show(engine, stats, dt, len(files))
        report[engine] = {'seconds': dt, 'documents': len(files), 'stats': {'|'.join(k): dict(v) for k, v in stats.items()}, 'silent_errors': errors[:200]}
        kinds = collections.Counter(row.get('refused', {}).get('code') or ('reader_error' if row.get('error') else 'ok') for row in rows)
        print('   outcomes: %s' % dict(kinds))
        if errors:
            print('   first silent errors:')
            for e in errors[:8]:
                print('     %s  %s  truth=%r  draft=%r' % e)
    if a.out:
        Path(a.out).write_text(json.dumps(report, indent=1), encoding='utf-8')


if __name__ == '__main__':
    main()
