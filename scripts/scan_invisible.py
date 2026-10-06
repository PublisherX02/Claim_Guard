"""Check the invisible-character guard against the committed result files for false positives.

    python scripts/scan_invisible.py [folder ...]      # default: outputs experiments

Reads every JSON and JSONL file under outputs/ and experiments/ (recorded model answers and replies, prompts, metrics and claims:
a wider net than the answers alone) and looks at every string in them. outputs/ is git-ignored apart from the evidence files that were
added on purpose, so run this on a fresh clone to scan exactly what is committed. A string containing a character that
`llm_adapter._INVISIBLE` rejects is counted as
  - "already garbled": the older guards (text in another alphabet, a replacement character, a long repetition) reject it anyway, so the
    new check changes nothing for it; or
  - "newly rejected": only the invisible-character check would reject it. These are the false-positive candidates.
The exit status is 1 if there is any "newly rejected" string.
"""
import json
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from llm_adapter import _FOREIGN_SCRIPT, _INVISIBLE, _REPETITION


def result_files(folders):
    found = []
    for folder in folders:
        for path in sorted((ROOT / folder).rglob('*')):
            if path.suffix in ('.json', '.jsonl') and path.is_file() and '.venv' not in path.parts:
                found.append(path.relative_to(ROOT).as_posix())
    return found


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


def main(argv):
    files = result_files(argv[1:] or ['outputs', 'experiments'])
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
    sys.exit(main(sys.argv))
