"""Checks the Rust exact interval against the Python oracle (src/workqueue/shadow.py) on a few thousand (k, n) pairs."""
import json, os, random, subprocess, sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, ROOT)
from src.workqueue.shadow import clopper_pearson  # noqa: E402

CG = os.environ.get("CG_BIN", os.path.join(ROOT, "rust", "target", "debug", "cg.exe"))


def main() -> int:
    rng = random.Random(11)
    cases = [(k, n) for n in (1, 2, 5, 10, 20, 40, 100) for k in range(0, n + 1)]
    cases += [(rng.randint(0, n), n) for n in (rng.randint(1, 400) for _ in range(1500))]
    proc = subprocess.run([CG, "xcheck"], input="".join(json.dumps({"op": "clopper_pearson", "k": k, "n": n}) + "\n" for k, n in cases), capture_output=True, text=True, check=True)
    rows = [json.loads(line) for line in proc.stdout.splitlines()]
    worst = 0.0
    for (k, n), got in zip(cases, rows):
        want = clopper_pearson(k, n, 0.95)
        assert got is not None and want[0] is not None, (k, n)
        worst = max(worst, abs(got[0] - want[0]), abs(got[1] - want[1]))
    print(f"{len(cases)} cases, largest difference {worst:.2e}")
    return 0 if worst < 1e-6 else 1


if __name__ == "__main__":
    raise SystemExit(main())
