#!/usr/bin/env python3
"""Throughput and latency of the explanation model behind any OpenAI-compatible server (vLLM, Ollama's /v1, llama.cpp).

    python rust/tools/model_bench.py --base http://127.0.0.1:11434/v1 --model gemma3:4b --name ollama_gemma3_4b
    python rust/tools/model_bench.py --base http://127.0.0.1:8000/v1 --model google/gemma-3-4b-it --name vllm_gemma3_4b --api-key $VLLM_API_KEY

It sends the kind of request the AI explanation step sends (a rule's text and a failure shape, no claim values), at rising concurrency, and writes
`outputs/defense/model_bench_<name>.json`: requests per minute, latency percentiles, output tokens per second, errors, and the GPU memory peak
(nvidia-smi, when present). The project's target is about 200 claims a minute on one card; the table says whether a server gets there.
Standard library only. Talks only to the address you give it.
"""
import argparse
import json
import statistics
import subprocess
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SYSTEM = ("You help a health-insurance claims reviewer. You are given one rule of a fictional rulebook and the way a claim failed it. "
          "Explain in two or three plain sentences why the finding matters and what the reviewer should check. Use only the rule text. "
          "Use the placeholders {value} and {line} where a claim value or a line would go. Never say a claim is approved or payable.")
RULES = [
    ("R007", "For each line, net_amount must equal quantity multiplied by unit_price, rounded to 2 decimals. A difference of at most 0.01 SAR passes.", "FAIL on {line}: net_amount {value}"),
    ("R012", "total_amount must equal the sum of the submitted line net_amount values, rounded to 2 decimals.", "FAIL: total_amount {value}"),
    ("R008", "For each service in policy.auth_required_services, its line authorization_id must be nonempty.", "FAIL on {line}: authorization_id is empty"),
    ("R009", "For authorization-required lines, resolve authorization_id in authorizations and match patient, service, status, dates and aggregate quantity.", "FAIL on {line}: quantity above the authorization maximum"),
    ("R014", "submission_date minus the latest service_date must be at most the policy's submission window in days.", "FAIL: submitted {value} days after the service"),
    ("R015", "currency must equal the policy currency (SAR). No conversion is performed.", "FAIL: currency {value}"),
]


def ask(base, model, key, i, max_tokens):
    rid, text, shape = RULES[i % len(RULES)]
    body = {"model": model, "temperature": 0, "max_tokens": max_tokens,
            "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": f"Rule {rid}: {text}\nFinding: {shape}\nExplain it for the reviewer."}]}
    req = urllib.request.Request(base.rstrip('/') + '/chat/completions', data=json.dumps(body).encode(), headers={'Content-Type': 'application/json', **({'Authorization': 'Bearer ' + key} if key else {})})
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            d = json.load(r)
        return time.time() - t, d.get('usage', {}).get('completion_tokens') or len(d['choices'][0]['message']['content'].split()), None
    except Exception as e:  # noqa: BLE001 - every failure is a data point
        return time.time() - t, 0, str(e)[:100]


def gpu_sampler(stop, peak):
    while not stop.is_set():
        try:
            out = subprocess.run(['nvidia-smi', '--query-gpu=memory.used,utilization.gpu', '--format=csv,noheader,nounits'], capture_output=True, text=True, timeout=5).stdout.strip().splitlines()[0]
            mem, util = [int(x) for x in out.split(',')]
            peak['mem'], peak['util'] = max(peak['mem'], mem), max(peak['util'], util)
        except Exception:  # noqa: BLE001
            pass
        stop.wait(0.5)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--base', required=True)
    ap.add_argument('--model', required=True)
    ap.add_argument('--name', required=True)
    ap.add_argument('--api-key', default='')
    ap.add_argument('--levels', default='1,2,4,8,16,32')
    ap.add_argument('--per-level', type=int, default=4, help='requests per level = this x concurrency (at least 16)')
    ap.add_argument('--max-tokens', type=int, default=160)
    a = ap.parse_args()
    host = a.base.split('//', 1)[-1].split('/', 1)[0].split(':')[0]
    ask(a.base, a.model, a.api_key, 0, 8)   # warm-up: loads the model into memory
    stop, peak = threading.Event(), {'mem': 0, 'util': 0}
    threading.Thread(target=gpu_sampler, args=(stop, peak), daemon=True).start()
    rows = []
    for conc in [int(x) for x in a.levels.split(',')]:
        n = max(16, conc * a.per_level)
        t0 = time.time()
        with ThreadPoolExecutor(conc) as pool:
            res = list(pool.map(lambda i: ask(a.base, a.model, a.api_key, i, a.max_tokens), range(n)))
        wall = time.time() - t0
        ok = [r for r in res if r[2] is None]
        lat = sorted(r[0] for r in ok) or [0]
        row = {'concurrency': conc, 'requests': n, 'errors': n - len(ok), 'requests_per_minute': round(len(ok) / wall * 60, 1),
               'latency_p50_s': round(statistics.median(lat), 2), 'latency_p95_s': round(lat[min(len(lat) - 1, int(len(lat) * 0.95))], 2),
               'output_tokens_per_second': round(sum(r[1] for r in ok) / wall, 1), 'seconds': round(wall, 1)}
        rows.append(row)
        print(json.dumps(row))
    stop.set()
    best = max(rows, key=lambda r: r['requests_per_minute'])
    report = {'server': a.base if host in ('127.0.0.1', 'localhost') else host, 'model': a.model, 'name': a.name, 'levels': rows, 'best_requests_per_minute': best['requests_per_minute'],
              'at_concurrency': best['concurrency'], 'gpu_memory_peak_mib': peak['mem'] or None, 'gpu_utilization_peak_percent': peak['util'] or None,
              'target_requests_per_minute': 200, 'reaches_target': best['requests_per_minute'] >= 200,
              'note': 'One explanation per request: about 250 prompt tokens in, up to %d out, temperature 0. Measured on this machine only; the target server is a different card.' % a.max_tokens}
    out = ROOT / 'outputs' / 'defense'
    out.mkdir(parents=True, exist_ok=True)
    (out / f'model_bench_{a.name}.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print('best %.1f requests/minute at concurrency %d; wrote model_bench_%s.json' % (best['requests_per_minute'], best['concurrency'], a.name))


if __name__ == '__main__':
    main()
