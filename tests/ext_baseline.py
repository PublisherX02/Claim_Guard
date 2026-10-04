"""How the extension tests prove the official engine is untouched: hashes of the official files and of the official results."""
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))

OFFICIAL_FILES = (
    'src/yara_engine.py', 'src/facts_extractor.py', 'src/engine_core.py', 'src/evaluate.py', 'rules/core.yar',
    'schemas/result.schema.json', 'rules/rules.json', 'rules/services.json', 'rules/policies.json', 'rules/diagnoses.json',
)
SPLITS = ('development', 'validation', 'stress')


def file_hash(rel):
    """SHA-256 of the file's lines joined with LF, so a CRLF checkout hashes the same as an LF one."""
    return hashlib.sha256(b'\n'.join((ROOT / rel).read_bytes().splitlines())).hexdigest()


def official_results_hash(split):
    from engine_core import config, load_jsonl
    from yara_engine import evaluate
    cfg = config(ROOT)
    h = hashlib.sha256()
    for claim in load_jsonl(ROOT / 'data' / split / 'claims.jsonl'):
        h.update(json.dumps(evaluate(claim, cfg, []), sort_keys=True, ensure_ascii=False).encode('utf-8'))
    return h.hexdigest()


def snapshot():
    return {'files': {f: file_hash(f) for f in OFFICIAL_FILES}, 'results': {s: official_results_hash(s) for s in SPLITS}}


if __name__ == '__main__':
    out = ROOT / 'tests' / 'fixtures' / 'extension_baseline.json'
    out.write_text(json.dumps(snapshot(), indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print(out)
