#!/usr/bin/env python3
"""Pack one run into a single zip to send back (docs/65 section 5), after checking it holds nothing it should not.

    python experiments/server/pack_results.py --run <name> [--out <file.zip>]

Includes every .json, samples.jsonl and report.md of outputs/server/<run>/. Before packing it scans the files for anything that looks like a secret
(a password or seed from the environment, a JWT, a private key header, an `.env` style key=value of the stack's secret names) and refuses if it finds one.
"""
import argparse
import os
import re
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import OUT_ROOT  # noqa: E402

SECRET_NAMES = ('JWT_SECRET', 'FERNET_KEY', 'DATA_KEY', 'PII_KEY', 'MONGO_PASSWORD', 'REDIS_PASSWORD', 'VLLM_API_KEY', 'HF_TOKEN', 'EMBEDDER_TOKEN', 'DOCUMENT_READER_TOKEN')
PATTERNS = [re.compile(r'eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}'), re.compile(r'-----BEGIN [A-Z ]*PRIVATE KEY-----'), re.compile(r'hf_[A-Za-z0-9]{30,}'),
            re.compile(r'(%s)\s*[=:]\s*\S{8,}' % '|'.join(SECRET_NAMES))]


def findings(text):
    out = [p.pattern[:30] for p in PATTERNS if p.search(text)]
    for name in ('CG_PASSWORD', 'CG_TOTP_SEED', 'CG_VIEW_PASSWORD', 'CG_VIEW_TOTP_SEED'):
        v = os.environ.get(name, '')
        if len(v) >= 6 and v in text:
            out.append(f'the value of {name}')
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--out', default='')
    a = ap.parse_args()
    d = OUT_ROOT / a.run
    files = sorted(p for p in d.iterdir() if p.is_file() and p.suffix in ('.json', '.jsonl', '.md'))
    if not files:
        raise SystemExit(f'nothing to pack in {d}')
    bad = {p.name: findings(p.read_text(encoding='utf-8', errors='replace')) for p in files}
    bad = {k: v for k, v in bad.items() if v}
    if bad:
        raise SystemExit('refusing to pack: possible secrets in ' + '; '.join(f'{k} ({", ".join(v)})' for k, v in bad.items()))
    out = Path(a.out or d.parent / f'{a.run}.zip')
    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
        for p in files:
            z.write(p, f'{a.run}/{p.name}')
    print(f'packed {len(files)} files, {out.stat().st_size / 1024:.0f} KiB, into {out}')


if __name__ == '__main__':
    main()
