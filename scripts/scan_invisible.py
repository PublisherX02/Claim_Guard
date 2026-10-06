"""Check the invisible-character guard against the committed result files for false positives.

    python scripts/scan_invisible.py

Reads every JSON and JSONL file git tracks under outputs/ and experiments/ (recorded model answers and replies, prompts, metrics and
claims: a wider net than the answers alone) and looks at every string in them. A string containing a character that
`llm_adapter._INVISIBLE` rejects is counted as
  - "already garbled": the older guards (text in another alphabet, a replacement character, a long repetition) reject it anyway, so the
    new check changes nothing for it; or
  - "newly rejected": only the invisible-character check would reject it. These are the false-positive candidates.
The exit status is 1 if there is any "newly rejected" string.
"""
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from llm_adapter import _FOREIGN_SCRIPT, _INVISIBLE, _REPETITION


def tracked_results():
    out = subprocess.run(['git', 'ls-files', '*.json', '*.jsonl'], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    return [f for f in out.splitlines() if f.startswith(('outputs/', 'experiments/'))]


def strings(node):
    if isinstance(node, str):
        yield node
    elif isinstance(node, dict):
        for v in node.values():
            yield from strings(v)
    elif isinstance(node, list):
        for v in node:
            yield from strings(v)


def documents(path):
    text = path.read_text(encoding='utf-8', errors='replace')
    if path.suffix == '.jsonl':
        for line in text.split('\n'):
            if line.strip():
                try:
                    yield json.loads(line)
                except ValueError:
                    yield line
    else:
        try:
            yield json.loads(text)
        except ValueError:
            yield text


def main():
    files = tracked_results()
    chars, seen, garbled, new = 0, 0, Counter(), Counter()
    for name in files:
        path = ROOT / name
        chars += len(path.read_text(encoding='utf-8', errors='replace'))
        for doc in documents(path):
            for s in strings(doc):
                seen += 1
                if not _INVISIBLE.search(s):
                    continue
                already = _FOREIGN_SCRIPT.search(s) or _REPETITION.search(s)
                (garbled if already else new)[name] += 1
    print(f'files: {len(files)}  characters: {chars:,}  strings: {seen:,}')
    print(f'strings with an invisible character: {sum(garbled.values()) + sum(new.values())}')
    print(f'  already garbled (older guards reject them anyway): {sum(garbled.values())}')
    print(f'  newly rejected (false-positive candidates): {sum(new.values())}')
    for name, n in sorted(new.items()):
        print(f'    {n} in {name}')
    return 1 if new else 0


if __name__ == '__main__':
    sys.exit(main())
