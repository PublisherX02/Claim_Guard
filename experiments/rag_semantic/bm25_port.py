"""Python port of rust/crates/cg-engine/src/rag.rs (corpus builder, tokenizer, BM25, thresholds), so retrievers can be compared on the same passages.

The port is only trustworthy if it reproduces the Rust result on the Rust test's own 26 questions (hit@1 88%, hit@3 96%, 5/5 off-topic refused);
`run.py` checks that before it prints anything else.
"""
import json
import math
import re
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STOP = {"a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "how", "if", "in", "is", "it", "of", "on", "or", "that", "the", "to",
        "what", "when", "which", "who", "with", "must", "should", "this", "there", "than", "then"}
K1, B, MIN_SCORE = 1.5, 0.75, 1.2
VERSION = 0


def stem(w):
    w = w.lower()
    for suf in ("ing", "ed", "es", "s"):
        if len(w) > len(suf) + 3 and w.endswith(suf):
            return w[: -len(suf)]
    return w


def tokens(text):
    out = []
    for raw in re.split(r"[^\w\-]", text):          # Rust: split on anything that is not alphanumeric, '-' or '_' (\w has '_')
        raw = raw.strip("-_")
        if not raw:
            continue
        lower = raw.lower()
        if "-" in lower or "_" in lower:
            out.append(lower)
            out.extend(stem(p) for p in re.split(r"[-_]", lower) if p and p not in STOP)
        elif lower not in STOP:
            out.append(stem(lower))
    return out


def sentences(text):
    out, cur = [], ""
    for part in re.findall(r".*?(?:\. |$)", text, flags=re.S):
        if not part:
            continue
        cur += part
        if len(cur) >= 60:
            out.append(cur.strip())
            cur = ""
    if cur.strip():
        out.append(cur.strip())
    return out


def num(v):
    return str(int(v)) if isinstance(v, (int, float)) and float(v).is_integer() else str(v)


def build_passages(rules, policies, version=VERSION):
    ps = []

    def add(source, kind, text):
        ps.append({"source": source, "kind": kind, "text": text, "tokens": tokens(f"{source} {text}")})

    for r in rules:
        rid, title = r["rule_id"], r["title"]
        src = f"rule {rid}"
        add(src, "title", f"{rid} {title}. Severity {r.get('severity', '')}.")
        for s in sentences(r.get("logic", "")):
            add(src, "what it checks", f"{rid} {title}: {s}")
        if r.get("corrective_action"):
            add(src, "what to do", f"{rid} {title}: {r['corrective_action']}")
    for pid, p in policies.items():
        src = f"policy {pid} (table version {version})"
        cur = p.get("currency", "")
        add(src, "policy", f"Policy {pid} of payer {p.get('payer_id', '')}: claims are in {cur} and must be submitted within {p.get('submission_window_days')} days of the latest service.")
        add(src, "policy", f"Policy {pid}: the allowed providers (the network) are {', '.join(p.get('allowed_providers', []))}.")
        add(src, "policy", f"Policy {pid}: services that need an authorization reference: {', '.join(p.get('auth_required_services', []))}.")
        for svc, t in (p.get("required_documents") or {}).items():
            add(src, "policy", f"Policy {pid}: service {svc} requires a supporting document of type {t}.")
        for svc, n in (p.get("max_unit_price") or {}).items():
            add(src, "policy", f"Policy {pid}: the highest unit price for {svc} is {num(n)} {cur}.")
        for svc, n in (p.get("max_quantity_per_line") or {}).items():
            add(src, "policy", f"Policy {pid}: the highest quantity on one line for {svc} is {num(n)}.")
    return ps


def load_corpus():
    rules = json.loads((ROOT / "rules" / "rules.json").read_text(encoding="utf-8"))
    policies = json.loads((ROOT / "rules" / "policies.json").read_text(encoding="utf-8"))
    return build_passages(rules, policies)


class BM25:
    def __init__(self, passages):
        self.ps = passages
        self.n = len(passages)
        self.df = Counter()
        for p in passages:
            for t in set(p["tokens"]):
                self.df[t] += 1
        self.avg = sum(len(p["tokens"]) for p in passages) / max(1, self.n)
        self.tf = [Counter(p["tokens"]) for p in passages]

    def raw_scores(self, question):
        """(score, matched, distinct_query_terms) for every passage; the Rust thresholds are applied by `search`."""
        q = tokens(question)
        seen, out = [], []
        for t in q:
            if t not in seen:
                seen.append(t)
        for i, p in enumerate(self.ps):
            score, matched = 0.0, 0
            for t in seen:
                f = self.tf[i].get(t)
                if not f:
                    continue
                matched += 1
                df = self.df.get(t, 1)
                idf = math.log((self.n - df + 0.5) / (df + 0.5) + 1.0)
                score += idf * f * (K1 + 1.0) / (f + K1 * (1.0 - B + B * len(p["tokens"]) / self.avg))
            out.append((score, matched, len(seen)))
        return out

    def search(self, question, k=3):
        if not tokens(question):
            return []
        scored = []
        for i, (score, matched, nq) in enumerate(self.raw_scores(question)):
            if score >= MIN_SCORE and matched >= min(nq, 2):
                scored.append((score, i))
        scored.sort(key=lambda x: (-x[0], x[1]))
        out, seen = [], set()
        for score, i in scored:
            p = self.ps[i]
            key = (p["source"], p["kind"])
            if key in seen:
                continue
            seen.add(key)
            out.append({"source": p["source"], "kind": p["kind"], "text": p["text"], "score": score})
            if len(out) >= k:
                break
        return out
