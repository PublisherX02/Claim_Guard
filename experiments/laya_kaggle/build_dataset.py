"""Build the Laya fine-tuning dataset from ClaimGuard's REAL labelled claims (no simulated reviewers).

One case = one (claim, rule) pair. The label is the organizers' answer key (data/*/expected_results.jsonl):
PASS / FAIL / UNABLE_TO_ASSESS / NOT_APPLICABLE. These are rule results, not reviewer decisions: this is a
proof of concept that Laya can learn the rulebook, and a first look at its calibrated confidence.

Split (by the organizers' own folders, so no claim is in two splits):
  train  = development (400 claims). PASS is capped so the rare classes are not drowned (see --max-pass).
  test   = validation (150 claims x 15 rules), complete.
  stress = stress (50 claims x 15 rules), complete (adversarial / edge cases).

Row schema is Laya's `{state, questions, gold}` (see laya.train.items_from_rows).
Run from the repository root:  python experiments/laya_kaggle/build_dataset.py
"""
import argparse
import json
import random
from collections import Counter
from pathlib import Path

STATUSES = ["PASS", "FAIL", "UNABLE_TO_ASSESS", "NOT_APPLICABLE"]

QUESTIONS = {
    "status": {
        "type": "choice",
        "instructions": "Apply the payer rule to the claim. What is the result of this rule check?",
        "criteria": {
            "PASS": "the claim satisfies the rule",
            "FAIL": "the claim violates the rule",
            "UNABLE_TO_ASSESS": "required data is missing or unknown, so the rule cannot be decided",
            "NOT_APPLICABLE": "the rule does not apply to this claim",
        },
    }
}


def read_jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def rows_for(split_dir, rules):
    claims = {c["claim_id"]: c for c in read_jsonl(split_dir / "claims.jsonl")}
    rows = []
    for r in read_jsonl(split_dir / "expected_results.jsonl"):
        claim = dict(claims[r["claim_id"]])
        claim.pop("schema_version", None)
        claim.pop("notes", None)  # boilerplate "Synthetic claim..." sentence
        if isinstance(claim.get("coverage"), dict):
            claim["coverage"] = {k: v for k, v in claim["coverage"].items() if k != "coverage_id"}  # id derived from claim_id, no signal
        rule = rules[r["rule_id"]]
        status = r["status"]
        # Compact JSON text: Laya cuts the state to a token budget, and the claim's tail (authorizations, attachments)
        # is what several rules need, so no whitespace is spent.
        state = json.dumps({"rule": {"id": rule["rule_id"], "title": rule["title"], "logic": rule["logic"]}, "claim": claim},
                           ensure_ascii=False, separators=(",", ":"))
        rows.append({
            "id": f'{r["claim_id"]}:{r["rule_id"]}',
            "claim_id": r["claim_id"],
            "rule_id": r["rule_id"],
            # the rule goes first so that, if a long claim is cut to the token budget, the cut falls on the claim's tail
            "state": state,
            "questions": QUESTIONS,
            "gold": {"status": {"label": status, "probabilities": {s: 1.0 if s == status else 0.0 for s in STATUSES}}},
        })
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="data")
    ap.add_argument("--rules", default="rules/rules.json")
    ap.add_argument("--out", default="experiments/laya_kaggle/dataset")
    ap.add_argument("--max-pass", type=int, default=1500, help="cap on PASS cases in train (all other classes kept)")
    ap.add_argument("--seed", type=int, default=20261015)
    a = ap.parse_args()

    rules = {r["rule_id"]: r for r in json.loads(Path(a.rules).read_text(encoding="utf-8"))}
    data, out = Path(a.data), Path(a.out)
    out.mkdir(parents=True, exist_ok=True)

    train = rows_for(data / "development", rules)
    passes = [r for r in train if r["gold"]["status"]["label"] == "PASS"]
    rest = [r for r in train if r["gold"]["status"]["label"] != "PASS"]
    random.Random(a.seed).shuffle(passes)
    train = rest + passes[: a.max_pass]
    random.Random(a.seed).shuffle(train)

    splits = {"train": train, "test": rows_for(data / "validation", rules), "stress": rows_for(data / "stress", rules)}
    summary = {}
    for name, rows in splits.items():
        with (out / f"{name}.jsonl").open("w", encoding="utf-8", newline="\n") as f:
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        summary[name] = {"cases": len(rows), "claims": len({r["claim_id"] for r in rows}),
                         "labels": dict(Counter(r["gold"]["status"]["label"] for r in rows))}
    # a claim must never appear in two splits
    ids = [{r["claim_id"] for r in rows} for rows in splits.values()]
    assert not (ids[0] & ids[1]) and not (ids[0] & ids[2]) and not (ids[1] & ids[2]), "claim leaked across splits"
    summary["note"] = ("Real labelled claims from the organizers' answer key; labels are rule results, not reviewer decisions. "
                       "Train PASS capped at %d; test and stress are complete." % a.max_pass)
    (out / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
