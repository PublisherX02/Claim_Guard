# 66. Server experiment results: run `server_20261101k`

Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261101k/`.

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
| Run started | 2026-11-01T18:16:26Z |

## 2. Scoreboard

| Experiment | Measure | Target | Result | Verdict |
|---|---|---|---|---|
| S3 | Rulebook questions answered with the right rule | >= 90 % | 92 % | PASS |
| S3 | Off-topic strings refused | all | 4 of 5 | FAIL |
| S0 | Peak memory of all containers vs server memory | <= 85 % | 39 % of 16.0 GiB | PASS |
| S0 | Peak GPU memory vs the card | <= 95 % | 43 % of 10 GiB | PASS |
| S0 | Containers that reached their memory limit | none | none | PASS |

4 of 5 measures pass; 1 fail.

Not run in this session: s06 (run it from another machine).

## 3. S0. Resources during the run

17 samples over 1 minutes (container memory and CPU from `docker stats`, GPU from `nvidia-smi`).

![GPU memory and utilisation over time](../../../docs/figures/server/server_20261101k_gpu.png)

![Peak memory by container against its limit](../../../docs/figures/server/server_20261101k_memory_peaks.png)

![Memory of all containers over time](../../../docs/figures/server/server_20261101k_memory_timeline.png)

| Container | Peak memory (MiB) | Limit (MiB) | Share of limit |
|---|---|---|---|
| claimguard-ollama-1 | 2434 | 5120 | 48 % |
| claimguard-embed-1 | 2091 | 4096 | 51 % |
| claimguard-mongo-1 | 1170 | 2048 | 57 % |
| claimguard-worker-1 | 442 | 3072 | 14 % |
| claimguard-api-1 | 206 | 4096 | 5 % |
| claimguard-reader-1 | 75 | 2048 | 4 % |
| claimguard-proxy-1 | 19 | 128 | 15 % |
| claimguard-redis-1 | 5 | 320 | 2 % |

GPU peaks: memory 4379 of 10240 MiB, utilisation 0 %, temperature 36 C, power 28 W.

The server never had less than 11.5 GiB of 16.0 GiB available.

## 4. S3. Ask the rulebook, by meaning

![Search quality and latency](../../../docs/figures/server/server_20261101k_search.png)

24 of 26 questions returned the right rule (92 %); 4 of 5 off-topic strings were refused. Rate-limited answers (429): 282. HTTP statuses of the quality pass: {"200": 26}.

| Questions | Concurrent | Count | Answered | Per second | Latency p50 (s) | Latency p95 (s) | From cache |
|---|---|---|---|---|---|---|---|
| cold | 1 | 78 | 30 | 0.6 | 1.201 | 4.822 | 0 |
| cold | 2 | 78 | 0 | 0.0 | - | - | 0 |
| cold | 4 | 78 | 0 | 0.0 | - | - | 0 |
| cold | 8 | 78 | 0 | 0.0 | - | - | 0 |
| warm (repeat of questions already asked) | 1 | 26 | 0 | 0.0 | - | - | 0 |

Questions that missed: "claim line without a service date" (wanted rule R001); "how many days after the service can a claim be submitted" (wanted rule R014, policy EDU-BASIC (table version 0), policy EDU-PLUS (table version 0)).

## 5. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.
