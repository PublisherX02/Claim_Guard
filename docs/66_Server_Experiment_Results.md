# 66. Server experiment results: run `server_20261101h`

Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261101h/`.

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
| Run started | 2026-11-01T15:23:24Z |

## 2. Scoreboard

| Experiment | Measure | Target | Result | Verdict |
|---|---|---|---|---|
| S1 | Explanations per minute (best level) | >= 200 | 209.9 at 16 concurrent | PASS |
| S1 | Model requests that failed | 0 | 0 | PASS |
| S2 | Claims per minute through the whole pipeline (best stage) | >= 200 | 4140.2 | PASS |
| S2 | Batches that failed | 0 | 0 | PASS |
| S2 | Slowest read request right after the load (95th percentile) | < 2 s | 2.73 s | FAIL |
| S3 | Rulebook questions answered with the right rule | >= 90 % | 92 % | PASS |
| S3 | Off-topic strings refused | all | 4 of 5 | FAIL |
| S4 | Fields read exactly from documents | >= 95 % | 90.0 % | FAIL |
| S4 | Documents that could not be read | 0 | 0 | PASS |
| S5 | Acknowledged claims lost in restarts | 0 | 0 | PASS |
| S5 | Services back after restart | all | 5 of 5 | PASS |
| S7 | health not down | yes | no | FAIL |
| S7 | no failing probe | yes | no | FAIL |
| S7 | audit chains verify | yes | no | FAIL |
| S7 | ledger verifies | yes | no | FAIL |
| S0 | Peak memory of all containers vs server memory | <= 85 % | 81 % of 16.0 GiB | PASS |
| S0 | Peak GPU memory vs the card | <= 95 % | 58 % of 10 GiB | PASS |
| S0 | Containers that reached their memory limit | none | claimguard-api-1, claimguard-ollama-1 | FAIL |
| S0 | Memory growth during the pipeline experiment | < 100 MiB/hour | 1116 MiB/h | FAIL |

10 of 19 measures pass; 9 fail.

Not run in this session: s06 (run it from another machine).

## 3. S1. The explanation model on the GPU

Model `gemma3:4b`, one explanation per request (about 250 tokens in, up to 160 out, temperature 0).

![Explanations per minute and latency against concurrency](figures/server/server_20261101h_model.png)

| Concurrent | Requests | Errors | Per minute | Latency p50 (s) | Latency p95 (s) | Tokens/s |
|---|---|---|---|---|---|---|
| 1 | 16 | 0 | 124.3 | 0.47 | 0.56 | 148.6 |
| 2 | 16 | 0 | 202.0 | 0.58 | 0.75 | 243.6 |
| 4 | 16 | 0 | 197.9 | 1.19 | 1.36 | 238.8 |
| 8 | 32 | 0 | 205.9 | 2.32 | 2.47 | 248.6 |
| 16 | 64 | 0 | 209.9 | 4.56 | 4.66 | 253.2 |
| 32 | 128 | 0 | 207.7 | 9.2 | 9.4 | 248.0 |
| 64 | 256 | 0 | 207.8 | 18.49 | 18.82 | 250.9 |

Best: **209.9 a minute** at 16 concurrent requests; GPU memory peak during the benchmark: 4379 MiB.

## 4. S2. The claim pipeline

Claims go through the proxy, the API, the queue, the engine and the store (POST /api/v1/queue/submit, asynchronous), in batches of rising size.

![Throughput and time per claim by batch size](figures/server/server_20261101h_pipeline.png)

| Batch | Batches | Claims in | Errors | Claims/min | Accept p50 (s) | Batch done p50 (s) | Batch done p95 (s) | ms per claim p50 |
|---|---|---|---|---|---|---|---|---|
| 10 | 6 | 60 | 0 | 1943.3 | 0.00 | 0.27 | 0.46 | 27 |
| 50 | 6 | 300 | 0 | 2931.8 | 0.01 | 1.04 | 1.16 | 21 |
| 200 | 4 | 800 | 0 | 3789.2 | 0.01 | 3.06 | 3.64 | 15 |
| 500 | 2 | 1000 | 0 | 4140.2 | 0.01 | 7.24 | 7.58 | 14 |

Answer time of three read requests (seconds), idle before the load and right after it:

| Request | Before p50 | Before p95 | After p50 | After p95 |
|---|---|---|---|---|
| dashboard | 1.917 | 2.007 | 1.929 | 2.633 |
| claims_list | 0.002 | 0.004 | 0.002 | 0.002 |
| health_panel | 2.041 | 3.163 | 2.249 | 2.728 |

Steady load (soak):

![Claims per minute and batch time during the soak](figures/server/server_20261101h_soak.png)

30 minute(s), 3000 claims, 0 failed batches.

## 5. S0. Resources during the run

507 samples over 42 minutes (container memory and CPU from `docker stats`, GPU from `nvidia-smi`).

![GPU memory and utilisation over time](figures/server/server_20261101h_gpu.png)

![Peak memory by container against its limit](figures/server/server_20261101h_memory_peaks.png)

![Memory of all containers over time](figures/server/server_20261101h_memory_timeline.png)

| Container | Peak memory (MiB) | Limit (MiB) | Share of limit |
|---|---|---|---|
| claimguard-ollama-1 | 5078 | 5120 | 99 % |
| claimguard-api-1 | 4103 | 4096 | 100 % |
| claimguard-worker-1 | 2544 | 3072 | 83 % |
| claimguard-mongo-1 | 1248 | 2048 | 61 % |
| claimguard-embed-1 | 1010 | 4096 | 25 % |
| claimguard-reader-1 | 206 | 2048 | 10 % |
| claimguard-proxy-1 | 35 | 128 | 27 % |
| claimguard-redis-1 | 7 | 320 | 2 % |

GPU peaks: memory 5909 of 10240 MiB, utilisation 100 %, temperature 69 C, power 329 W.

The server never had less than 3.4 GiB of 16.0 GiB available.

Memory trend of the whole stack during S2: +1116 MiB per hour (a straight-line fit; a steady climb over a long soak means a leak, a flat line does not).

## 6. S3. Ask the rulebook, by meaning

![Search quality and latency](figures/server/server_20261101h_search.png)

24 of 26 questions returned the right rule (92 %); 4 of 5 off-topic strings were refused. Rate-limited answers (429): 295. HTTP statuses of the quality pass: {"200": 26}.

| Concurrent | Questions | Answered | Per second | Latency p50 (s) | Latency p95 (s) |
|---|---|---|---|---|---|
| 1 | 78 | 17 | 104.7 | 0.002 | 0.002 |
| 2 | 78 | 0 | 0.0 | - | - |
| 4 | 78 | 0 | 0.0 | - | - |
| 8 | 78 | 0 | 0.0 | - | - |

Questions that missed: "claim line without a service date" (wanted rule R001); "how many days after the service can a claim be submitted" (wanted rule R014, policy EDU-BASIC (table version 0), policy EDU-PLUS (table version 0)).

## 7. S4. Documents to claims

![Document reading accuracy and speed](figures/server/server_20261101h_documents.png)

40 documents tried, 0 failed, 0 waits because the reader was busy.

| Condition | Documents | Field accuracy | Documents fully right | Seconds p50 | Seconds p95 |
|---|---|---|---|---|---|
| digital PDF | 10 | 100.0 % | 100 % | 0.0 | 0.0 |
| image-only PDF | 10 | 100.0 % | 100 % | 3.1 | 3.2 |
| photo | 10 | 60.0 % | 60 % | 2.3 | 7.6 |
| scan | 10 | 100.0 % | 100 % | 3.2 | 5.5 |

For comparison: docs/45: 97.4 % of fields, 66.7 % of documents fully right, two readers, on the build PC.

## 8. S5. Restarts under load

![Seconds of unavailability per restarted service](figures/server/server_20261101h_s5_restarts.png)

| Service | Restart command (s) | Unhealthy (s) | Recovered | Sent | Acknowledged | Failed during restart | Acknowledged but missing |
|---|---|---|---|---|---|---|---|
| worker | 2.7 | 0.0 | yes | 375 | 375 | 0 | 0 |
| api | 5.7 | 5.4 | yes | 280 | 255 | 25 | 0 |
| redis | 0.4 | 0.0 | yes | 335 | 335 | 0 | 0 |
| mongo | 0.5 | 0.0 | yes | 315 | 315 | 0 | 0 |
| proxy | 0.5 | 0.5 | yes | 185 | 175 | 10 | 0 |

## 9. S7. Integrity and backup

Health panel: None (2.50 s). Audit verification: HTTP 502. Ledger: HTTP 502.

## 10. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.
