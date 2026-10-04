"""Run the fuzz suites at the 'deep' profile and record the evidence.

    python scripts/fuzz_campaign.py --examples 3000 --out outputs/defense/fuzz.json

Each surface is one tests/test_fuzz_*.py module run in its own process. Exit code 1 if any surface fails.
"""
import argparse
import io
import json
import sys
import time
import unittest
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs  # noqa: E402

SURFACES = ['test_fuzz_ingest', 'test_fuzz_llm_output', 'test_fuzz_audit_log', 'test_fuzz_engine',
            'test_fuzz_injection', 'test_fuzz_review_page', 'test_fuzz_access']


def run_surface(module, examples):
    """Run one tests/<module>.py in this process at the 'deep' profile, then restore the previous profile.
    (In-process on purpose: tests/test_security_owasp.py bans process-spawning imports in scripts/.)"""
    previous = fs.ACTIVE
    fs.use_profile('deep', examples)
    sys.modules.pop(module, None)             # settings bind at definition, so the module must be imported afresh
    t0 = time.monotonic()
    stream = io.StringIO()
    try:
        suite = unittest.TestLoader().discover(str(ROOT / 'tests'), pattern=f'{module}.py', top_level_dir=str(ROOT / 'tests'))
        result = unittest.TextTestRunner(stream=stream, verbosity=0).run(suite)
    finally:
        fs.use_profile(previous)
        sys.modules.pop(module, None)         # do not leave deep-profile tests behind for a later importer
    ok = result.wasSuccessful() and result.testsRun > 0  # a pattern that matches nothing runs 0 tests and succeeds
    return {'status': 'pass' if ok else 'fail', 'tests': result.testsRun, 'seconds': round(time.monotonic() - t0, 1),
            'examples_per_test': examples, 'output_tail': stream.getvalue()[-2000:]}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--examples', type=int, default=3000)
    ap.add_argument('--surfaces', nargs='*', default=SURFACES)
    ap.add_argument('--out', default='outputs/defense/fuzz.json')
    a = ap.parse_args(argv)
    report = {'started': datetime.now(timezone.utc).isoformat(), 'profile': 'deep', 'surfaces': {}}
    for m in a.surfaces:
        report['surfaces'][m] = run_surface(m, a.examples)
        print(m, report['surfaces'][m]['status'], f"{report['surfaces'][m]['tests']} tests", f"{report['surfaces'][m]['seconds']}s")
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2), encoding='utf-8')
    return 0 if all(s['status'] == 'pass' for s in report['surfaces'].values()) else 1


if __name__ == '__main__':
    sys.exit(main())
