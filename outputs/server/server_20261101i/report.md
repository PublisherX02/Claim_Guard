# 66. Server experiment results: run `server_20261101i`

Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261101i/`.

## 1. The machine

| Item | Value |
|---|---|
| Host | khalil |
| Operating system | Linux-7.0.14-17-pve-x86_64-with-glibc2.43 |
| CPU | AMD Ryzen 7 5700X 8-Core Processor (8 threads) |
| Memory | 16.0 GiB |
| GPU | NVIDIA GeForce RTX 3080, 10240 MiB, 615.71.09 |
| Docker | Docker version 29.9.2, build a82e528 |
| Compose | 5.6.1 |
| Code version | not available |
| Run started | 2026-11-01T17:44:44Z |

## 2. Scoreboard

| Experiment | Measure | Target | Result | Verdict |
|---|---|---|---|---|
| S2 | Claims per minute through the whole pipeline (best stage) | >= 200 | 3149.3 | PASS |
| S2 | Batches that failed | 0 | 0 | PASS |
| S2 | Slowest read request right after the load (95th percentile) | < 2 s | 2.03 s | FAIL |
| S7 | health not down | yes | yes | PASS |
| S7 | no failing probe | yes | no | FAIL |
| S7 | audit chains verify | yes | yes | PASS |
| S7 | ledger verifies | yes | yes | PASS |
| S0 | Peak memory of all containers vs server memory | <= 85 % | 33 % of 16.0 GiB | PASS |
| S0 | Peak GPU memory vs the card | <= 95 % | 43 % of 10 GiB | PASS |
| S0 | Containers that reached their memory limit | none | none | PASS |
| S0 | Memory growth during the pipeline experiment | < 100 MiB/hour | 191 MiB/h | FAIL |

8 of 11 measures pass; 3 fail.

Not run in this session: s06 (run it from another machine).

## 3. S2. The claim pipeline

Claims go through the proxy, the API, the queue, the engine and the store (POST /api/v1/queue/submit, asynchronous), in batches of rising size.

![Throughput and time per claim by batch size](../../../docs/figures/server/server_20261101i_pipeline.png)

| Batch | Batches | Claims in | Errors | Claims/min | Accept p50 (s) | Batch done p50 (s) | Batch done p95 (s) | ms per claim p50 |
|---|---|---|---|---|---|---|---|---|
| 10 | 6 | 60 | 0 | 2314.1 | 0.00 | 0.26 | 0.26 | 26 |
| 50 | 6 | 300 | 0 | 2942.1 | 0.00 | 1.02 | 1.02 | 20 |
| 200 | 4 | 800 | 0 | 3149.3 | 0.00 | 3.81 | 3.81 | 19 |
| 500 | 2 | 1000 | 0 | 3111.9 | 0.01 | 9.63 | 9.86 | 19 |

Answer time of three read requests (seconds), idle before the load and right after it:

| Request | Before p50 | Before p95 | After p50 | After p95 |
|---|---|---|---|---|
| dashboard | 1.643 | 1.733 | 1.636 | 2.026 |
| claims_list | 0.002 | 0.002 | 0.002 | 0.003 |
| health_panel | 1.768 | 2.863 | 1.784 | 1.852 |

Steady load (soak):

![Claims per minute and batch time during the soak](../../../docs/figures/server/server_20261101i_soak.png)

10 minute(s), 1000 claims, 0 failed batches.

## 4. S0. Resources during the run

144 samples over 12 minutes (container memory and CPU from `docker stats`, GPU from `nvidia-smi`).

![GPU memory and utilisation over time](../../../docs/figures/server/server_20261101i_gpu.png)

![Peak memory by container against its limit](../../../docs/figures/server/server_20261101i_memory_peaks.png)

![Memory of all containers over time](../../../docs/figures/server/server_20261101i_memory_timeline.png)

| Container | Peak memory (MiB) | Limit (MiB) | Share of limit |
|---|---|---|---|
| claimguard-ollama-1 | 2023 | 5120 | 40 % |
| claimguard-mongo-1 | 1199 | 2048 | 59 % |
| claimguard-worker-1 | 983 | 3072 | 32 % |
| claimguard-embed-1 | 832 | 4096 | 20 % |
| claimguard-api-1 | 201 | 4096 | 5 % |
| claimguard-reader-1 | 80 | 2048 | 4 % |
| claimguard-proxy-1 | 21 | 128 | 16 % |
| claimguard-redis-1 | 6 | 320 | 2 % |

GPU peaks: memory 4371 of 10240 MiB, utilisation 0 %, temperature 37 C, power 28 W.

The server never had less than 11.4 GiB of 16.0 GiB available.

Memory trend of the whole stack during S2: +191 MiB per hour (a straight-line fit; a steady climb over a long soak means a leak, a flat line does not).

## 5. S7. Integrity and backup

Health panel: degraded (2.12 s). Audit verification: HTTP 200. Ledger: HTTP 200.

| Health probe | Status |
|---|---|
| api | ok |
| user_store | ok |
| audit_logs | ok |
| disk | ok |
| anchor | ok |
| timestamp | unknown |
| ledger | ok |
| signals | ok |
| queue_store | ok |
| broker | ok |
| scheduler | ok |
| audit_guard | ok |
| outbox | ok |
| flow | degraded |
| model | ok |

## 6. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.
