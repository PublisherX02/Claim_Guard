# 66. Server experiment results: run `server_20261101m`

Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261101m/`.

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
| Run started | 2026-11-01T22:42:38Z |

## 2. Scoreboard

| Experiment | Measure | Target | Result | Verdict |
|---|---|---|---|---|
| S1 | Explanations per minute (best level) | >= 200 | 214.6 at 64 concurrent | PASS |
| S1 | Model requests that failed | 0 | 0 | PASS |
| S2 | Claims per minute through the whole pipeline (best stage) | >= 200 | 8551.3 | PASS |
| S2 | Batches that failed | 0 | 0 | PASS |
| S2 | Slowest read request right after the load (95th percentile) | < 2 s | 0.36 s | PASS |
| S3 | Rulebook questions answered with the right rule | >= 90 % | 92 % | PASS |
| S3 | Off-topic strings refused | all | 4 of 5 | FAIL |
| S4 | Fields read exactly from documents | >= 95 % | 90.0 % | FAIL |
| S4 | Documents that could not be read | 0 | 0 | PASS |
| S5 | Acknowledged claims lost in restarts | 0 | 0 | PASS |
| S5 | Services back after restart | all | 5 of 5 | PASS |
| S7 | health not down | yes | yes | PASS |
| S7 | no failing probe | yes | no | FAIL |
| S7 | audit chains verify | yes | yes | PASS |
| S7 | ledger verifies | yes | yes | PASS |
| S0 | Peak memory of all containers vs server memory | <= 85 % | 50 % of 16.0 GiB | PASS |
| S0 | Peak GPU memory vs the card | <= 95 % | 58 % of 10 GiB | PASS |
| S0 | Containers that reached their memory limit | none | claimguard-ollama-1 | FAIL |
| S0 | Memory growth during the pipeline experiment | < 100 MiB/hour | -241 MiB/h | PASS |

15 of 19 measures pass; 4 fail.

Not run in this session: s06 (run it from another machine).

## 3. S1. The explanation model on the GPU

Model `gemma3:4b`, one explanation per request (about 250 tokens in, up to 160 out, temperature 0).

![Explanations per minute and latency against concurrency](figures/server/server_20261101m_model.png)

| Concurrent | Requests | Errors | Per minute | Latency p50 (s) | Latency p95 (s) | Tokens/s |
|---|---|---|---|---|---|---|
| 1 | 16 | 0 | 116.6 | 0.51 | 0.61 | 139.5 |
| 2 | 16 | 0 | 189.0 | 0.63 | 0.78 | 228.0 |
| 4 | 16 | 0 | 188.5 | 1.23 | 1.39 | 227.3 |
| 8 | 32 | 0 | 189.8 | 2.5 | 2.63 | 229.8 |
| 16 | 64 | 0 | 189.8 | 5.02 | 5.17 | 229.0 |
| 32 | 128 | 0 | 188.5 | 10.16 | 10.35 | 229.2 |
| 64 | 256 | 0 | 214.6 | 17.68 | 18.13 | 263.4 |

Best: **214.6 a minute** at 64 concurrent requests; GPU memory peak during the benchmark: 4379 MiB.

## 4. S2. The claim pipeline

Claims go through the proxy, the API, the queue, the engine and the store (POST /api/v1/queue/submit, asynchronous), in batches of rising size.

![Throughput and time per claim by batch size](figures/server/server_20261101m_pipeline.png)

| Batch | Batches | Claims in | Errors | Claims/min | Accept p50 (s) | Batch done p50 (s) | Batch done p95 (s) | ms per claim p50 |
|---|---|---|---|---|---|---|---|---|
| 10 | 6 | 60 | 0 | 2321.8 | 0.00 | 0.26 | 0.26 | 26 |
| 50 | 6 | 300 | 0 | 5847.9 | 0.00 | 0.51 | 0.51 | 10 |
| 200 | 4 | 800 | 0 | 8551.3 | 0.00 | 1.40 | 1.53 | 7 |
| 500 | 2 | 1000 | 0 | 8122.3 | 0.01 | 3.69 | 3.80 | 7 |

Answer time of three read requests (seconds), idle before the load and right after it:

| Request | Before p50 | Before p95 | After p50 | After p95 |
|---|---|---|---|---|
| dashboard | 0.030 | 0.032 | 0.196 | 0.219 |
| claims_list | 0.002 | 0.002 | 0.002 | 0.002 |
| health_panel | 0.154 | 0.377 | 0.338 | 0.362 |

Steady load (soak):

![Claims per minute and batch time during the soak](figures/server/server_20261101m_soak.png)

30 minute(s), 3000 claims, 0 failed batches.

## 5. S0. Resources during the run

734 samples over 61 minutes (container memory and CPU from `docker stats`, GPU from `nvidia-smi`).

![GPU memory and utilisation over time](figures/server/server_20261101m_gpu.png)

![Peak memory by container against its limit](figures/server/server_20261101m_memory_peaks.png)

![Memory of all containers over time](figures/server/server_20261101m_memory_timeline.png)

| Container | Peak memory (MiB) | Limit (MiB) | Share of limit |
|---|---|---|---|
| claimguard-ollama-1 | 5029 | 5120 | 98 % |
| claimguard-mongo-1 | 1130 | 2048 | 55 % |
| claimguard-embed-1 | 1044 | 4096 | 25 % |
| claimguard-worker-1 | 973 | 3072 | 32 % |
| claimguard-api-1 | 230 | 4096 | 6 % |
| claimguard-reader-1 | 129 | 2048 | 6 % |
| claimguard-proxy-1 | 24 | 128 | 18 % |
| claimguard-redis-1 | 8 | 320 | 3 % |

GPU peaks: memory 5909 of 10240 MiB, utilisation 98 %, temperature 68 C, power 328 W.

The server never had less than 7.7 GiB of 16.0 GiB available.

Memory trend of the whole stack during S2: -241 MiB per hour (a straight-line fit; a steady climb over a long soak means a leak, a flat line does not).

## 6. S3. Ask the rulebook, by meaning

![Search quality and latency](figures/server/server_20261101m_search.png)

24 of 26 questions returned the right rule (92 %); 4 of 5 off-topic strings were refused. Rate-limited answers (429): 0. HTTP statuses of the quality pass: {"200": 26}.

| Questions | Concurrent | Count | Answered | Per second | Latency p50 (s) | Latency p95 (s) | From cache |
|---|---|---|---|---|---|---|---|
| cold | 1 | 78 | 78 | 0.2 | 4.800 | 5.483 | 0 |
| cold | 2 | 78 | 78 | 0.3 | 7.990 | 10.385 | 0 |
| cold | 4 | 78 | 78 | 0.2 | 17.423 | 21.047 | 0 |
| cold | 8 | 78 | 74 | 0.2 | 36.767 | 43.620 | 0 |
| warm (repeat of questions already asked) | 1 | 26 | 26 | 349.8 | 0.002 | 0.005 | 26 |

Questions that missed: "claim line without a service date" (wanted rule R001); "how many days after the service can a claim be submitted" (wanted rule R014, policy EDU-BASIC (table version 0), policy EDU-PLUS (table version 0)).

## 7. S4. Documents to claims

![Document reading accuracy and speed](figures/server/server_20261101m_documents.png)

40 documents tried, 0 failed, 0 waits because the reader was busy.

| Condition | Documents | Field accuracy | Documents fully right | Seconds p50 | Seconds p95 |
|---|---|---|---|---|---|
| digital PDF | 10 | 100.0 % | 100 % | 0.0 | 0.1 |
| image-only PDF | 10 | 100.0 % | 100 % | 3.0 | 3.2 |
| photo | 10 | 60.0 % | 60 % | 2.3 | 6.9 |
| scan | 10 | 100.0 % | 100 % | 3.2 | 5.6 |

For comparison: docs/45: 97.4 % of fields, 66.7 % of documents fully right, two readers, on the build PC.

## 8. S5. Restarts under load

![Seconds of unavailability per restarted service](figures/server/server_20261101m_s5_restarts.png)

| Service | Restart command (s) | Unhealthy (s) | Recovered | Sent | Acknowledged | Failed during restart | Acknowledged but missing |
|---|---|---|---|---|---|---|---|
| worker | 1.2 | 0.0 | yes | 410 | 410 | 0 | 0 |
| api | 5.5 | 5.4 | yes | 315 | 295 | 20 | 0 |
| redis | 0.4 | 0.0 | yes | 375 | 375 | 0 | 0 |
| mongo | 0.5 | 0.0 | yes | 360 | 360 | 0 | 0 |
| proxy | 0.4 | 0.5 | yes | 200 | 195 | 5 | 0 |

## 9. S7. Integrity and backup

Health panel: degraded (0.92 s). Audit verification: HTTP 200. Ledger: HTTP 200.

| Health probe | Status |
|---|---|
| api | ok |
| user_store | ok |
| audit_logs | ok |
| disk | ok |
| anchor | ok |
| timestamp | unknown |
| ledger | ok |
| signals | degraded |
| queue_store | ok |
| broker | ok |
| scheduler | ok |
| audit_guard | ok |
| outbox | ok |
| flow | degraded |
| model | ok |

## 10. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.

## 11. Earlier run (`server_20261101h`), full tables

The tables of the second run, kept for comparison. The first run (`server_20261031`) is in the first-edition report.


Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261101h/`.

### 1. The machine

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

### 2. Scoreboard

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

### 3. S1. The explanation model on the GPU

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

### 4. S2. The claim pipeline

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

### 5. S0. Resources during the run

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

### 6. S3. Ask the rulebook, by meaning

![Search quality and latency](figures/server/server_20261101h_search.png)

24 of 26 questions returned the right rule (92 %); 4 of 5 off-topic strings were refused. Rate-limited answers (429): 295. HTTP statuses of the quality pass: {"200": 26}.

| Concurrent | Questions | Answered | Per second | Latency p50 (s) | Latency p95 (s) |
|---|---|---|---|---|---|
| 1 | 78 | 17 | 104.7 | 0.002 | 0.002 |
| 2 | 78 | 0 | 0.0 | - | - |
| 4 | 78 | 0 | 0.0 | - | - |
| 8 | 78 | 0 | 0.0 | - | - |

Questions that missed: "claim line without a service date" (wanted rule R001); "how many days after the service can a claim be submitted" (wanted rule R014, policy EDU-BASIC (table version 0), policy EDU-PLUS (table version 0)).

### 7. S4. Documents to claims

![Document reading accuracy and speed](figures/server/server_20261101h_documents.png)

40 documents tried, 0 failed, 0 waits because the reader was busy.

| Condition | Documents | Field accuracy | Documents fully right | Seconds p50 | Seconds p95 |
|---|---|---|---|---|---|
| digital PDF | 10 | 100.0 % | 100 % | 0.0 | 0.0 |
| image-only PDF | 10 | 100.0 % | 100 % | 3.1 | 3.2 |
| photo | 10 | 60.0 % | 60 % | 2.3 | 7.6 |
| scan | 10 | 100.0 % | 100 % | 3.2 | 5.5 |

For comparison: docs/45: 97.4 % of fields, 66.7 % of documents fully right, two readers, on the build PC.

### 8. S5. Restarts under load

![Seconds of unavailability per restarted service](figures/server/server_20261101h_s5_restarts.png)

| Service | Restart command (s) | Unhealthy (s) | Recovered | Sent | Acknowledged | Failed during restart | Acknowledged but missing |
|---|---|---|---|---|---|---|---|
| worker | 2.7 | 0.0 | yes | 375 | 375 | 0 | 0 |
| api | 5.7 | 5.4 | yes | 280 | 255 | 25 | 0 |
| redis | 0.4 | 0.0 | yes | 335 | 335 | 0 | 0 |
| mongo | 0.5 | 0.0 | yes | 315 | 315 | 0 | 0 |
| proxy | 0.5 | 0.5 | yes | 185 | 175 | 10 | 0 |

### 9. S7. Integrity and backup

Health panel: None (2.50 s). Audit verification: HTTP 502. Ledger: HTTP 502.

### 10. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.


## 12. Review notes

### On the third run (`server_20261101m`, full run on the final version, PR #43)
The server was updated to the code of PR #43 and rebuilt from nothing, then every experiment was run again (S1 to S5, S7 on the server, S6 from outside). 15 of 19 measures pass. The 31-page third edition (`docs/reports/ClaimGuard_Server_Report_v3.pdf` covers runs `h` to `l`) and the fourth edition `docs/reports/ClaimGuard_Server_Report_v4.pdf` explain it.
* **Speed:** S1 best 214.6 claims/min at 64 at once (single request 116.6/min, median 0.51 s). S2 stages 2,322 / 5,848 / 8,551 / 8,122 claims/min at 10 / 50 / 200 / 500 at once; a 30-minute soak of 3,000 claims had 0 errors.
* **Memory:** API 230 MiB at its peak (it was 4,103 MiB before PR #39); worker 973 MiB; Ollama 5,029 of 5,120 MiB (98 %, the tightest limit left).
* **S3 (honest speed):** 24 of 26 questions right (92 %), off-topic refused 4 of 5. A new question takes a median 3.74 s (p95 5.34 s); eight asked at once on a cold cache take up to 36.8 s and 4 of 78 failed; a repeated question costs 2.3 ms because it comes from the answer cache.
* **S4:** 90.0 % (digital 100, image-only 100, scan 100, photo 60). Photos stay at 60 %; `OCR_DPI` 200 is untried.
* **S5:** 0 claims lost in the restarts; API unavailable 5.4 s, proxy 0.5 s.
* **S6:** 7 of 8 (from the PC, through Cloudflare).
* **S7:** audit chain ok over 32,154 events; 109 ledger checkpoints; probes `flow` and `signals` were degraded and `timestamp` unknown (no TSA set). Degraded `flow` and `signals` come from the thousands of test claims and the test accounts' failed sign-ins; they clear when those are gone. The database was cleaned afterwards (no claims left) for the jury.
* **Run `n`:** S2 repeated on about 9,160 claims: dashboard 0.302 to 0.319 s, health 0.472 to 0.496 s, API 207 MiB, worker 954 MiB.


### On the second run (`server_20261101h`)
The 22-page second edition `docs/reports/ClaimGuard_Server_Report.pdf` explains this run: the explanation model moved from vLLM to Ollama so the vision model fits on the 10 GB card, photos rose from 50 % to 60 %, nothing was lost in five restarts. Two things the figures above leave open:
* **S3 speed was not measured** (295 answers were refused by the rate limit). The 26 quality questions took a median of 3.8 s each. This agrees with an outside measurement of the live site on the same day (31 new questions, median 4.6 s, slowest 6.1 s, all answered by the meaning search with no model step), and it is not the 2 ms of the first run: that run asked the same 26 questions three times and measured the answer cache. Where the seconds go is not yet known (embedding on 2 CPU cores is the main suspect); `s03_search.py` now asks new questions and saves each answer's tier and timing. 
* **API memory and the 2 s dashboard** (S0, S2): see "Found after go-live" below for the code reading and the change.

### On the first run (`server_20261031`)
The first-edition report explained that run. What its scoreboard did not say:

* **S6, "internal ports closed" = no (port 8080):** the scan went through Cloudflare, which answers on 8080 itself. On the server `ss -tlnp` shows only Caddy on 80 and 443. The scan therefore tested Cloudflare's edge; the server's own exposure (firewall, direct access by its address, encryption between Cloudflare and Caddy) is checked separately: docs/47, "Behind Cloudflare".
* **S7, "no failing probe" = no:** the `signals` probe counts failed sign-ins and the test accounts produced several; it clears after about an hour. `timestamp` is "unknown" because no timestamp authority (`TSA_URL`) is set.
* **S4, photos 50 %:** only Tesseract ran. The vision model (`baidu/Unlimited-OCR`, 88.1 % on photos on the build PC) was not part of the install, which only adds it when `CG_VLM_MODEL` is given. It does not fit on the 10 GB card beside the explanation model: docs/47, "The vision model".
* **S0, API memory 90 % of its limit:** the API keeps claims in memory; it was killed at the old 768 MB limit with about 7,000 queued claims and reached 2.77 GiB with about 8,500. The limit is now 3 GB as a stopgap. Reading the code did not confirm that the API holds the claims in memory (they are in MongoDB). It did find work that grows with the audit logs: checking a chain, the security-signals probe, the signed ledger and the payer-rules refresh each read a whole log file into memory, with a decrypted copy of every row, and the anchor check read the file twice; the health panel runs them. They now read one line at a time (`cg_audit::for_each_row`), so their memory no longer depends on the size of the log. Whether this was the main cause is not yet shown: the sizes of the audit logs on the server and a new memory measurement decide it.
* **S0, GPU 77 %:** vLLM reserves 85 % of the card (the value that worked; 0.55 did not start), so the 77 % is not free room.
* **S0, sampling:** the memory timeline has no samples between about minute 70 and 115, so the memory-growth criterion (under 100 MiB an hour) is not verified by this run.
* **S5, "0 lost":** counted as the queue total before and after, not claim by claim; a loss could be hidden by a claim that was refused to the client yet stored. The old read-back could not see queued claims at all (see the correction in `s05_resilience.py`).
* **Raw files:** `outputs/` is git-ignored by design; the second run's raw files were added by force. The figures and this chapter are the record.

## Found after go-live (1 November 2026)
* **Buttons answered "Request failed (unsupported_media_type)".** Every console button that posts without a form (sign out, claim, release, and others) failed behind Cloudflare and Caddy, for every role. The proxy forwards such a request as `Transfer-Encoding: chunked` with nothing in it; the API counted the header as "a body" and then refused it for having no JSON content type. Reproduced against the live site with `curl -X POST /api/v1/auth/logout` (415). Fixed in the API: only a real body counts (`cg-api` `guarded`), with a regression test. Why the experiments missed it: they always send a JSON body, and the build PC had no proxy in front.
* **"Ask the rulebook" gave no sign of work.** The page now shows a spinner at once, disables the button, and after 4 seconds says it is still working.
* **The Brand page was open to reviewers.** It is the identity sheet for the people who run the system; menu entry and route now need the administrator permission (`users.manage`).
* **S3 speed measured the answer cache.** The first run's speed table asked the same 26 questions three times and the server answers a repeated question from its cache, so 2 ms is not the cost of a new question. Measured from outside on the live site (one person, 31 different questions): median 4.6 s, slowest 6.1 s, none of them model-written, so the cause is on the server before any answer is written (embedding on 2 CPU cores, a rewrite step or the API; not yet known). `s03_search.py` now asks new questions (time stamp added), also runs the repeat for contrast, and saves each answer's `cached`, `tier`, `planner` and `timing` fields; the report prints them. The second run did not repeat the speed test (rate limit).
* **API memory (second run, server_20261101h): probable cause found in the code.** With about 15,000 stored claims the API sat at its 4 GiB limit, grew about 1.1 GiB an hour while claims were added, and the dashboard and health panel took about 2 s even when idle. The audit-log streaming of the first fix does not touch this. Several routines read **every claim document, collected whole in memory**: the dashboard and the health probe "flow" (up to 5,000 documents from each of two states per call), the live board and the daily report (`stats_snapshot`: every claim in every state, up to 100,000 per state) and the periodic audit `reconcile` (the same). Each claim document carries the receipt, all rule results, the decisions and the event history, so thousands of them are hundreds of MB, copied again as `Value` trees. These now read one claim at a time (`QueueStore::for_each_in_state`; the database store walks a cursor) and keep only counts and totals. The result is the same numbers with memory that no longer grows with the number of claims; not yet measured on the server. Still collected whole, not used by the health panel or the board: the routing-style report behind the light-review set (`workload::report`, cached), `reliability::learn` and `replay_report`, the feedback report, shadow and replay. They run rarely, but should follow if the API still grows.
* **Dashboard time grows with the stored claims (third server report, runs i to l):** 3 ms on an empty database, 0.6 s with about 2,300 claims, 1.6 to 1.8 s with 15,000 to 18,000; the API's memory stayed near 205 MiB. So the time is reading and decrypting every claim, not the load. The dashboard and the health probe "flow" now read only the queue's own fields (`QueueStore::for_each_in_state_light`: the database sends a projection of `claim_id`, `version`, `state_at`, `escalated`, `signoff`, `receipt.eligibility`, so no claim body is read or decrypted), and the live board's numbers (`stats`) are reused for at most 30 seconds while the claim counts by state and the routing version have not changed (it reads every claim in full, and the board now refreshes when a claim moves). Not yet measured on the server.
* **Where the 2 s goes** was not measured: reading 10,000 full documents costs time as well as memory; if the dashboard is still slow after this change, the next step is to read only the fields it needs (a projection) or to keep running counts.
* **"flow" degraded**: with thousands of claims waiting and nobody on shift, this probe reports "a claim has waited more than 30 minutes" and the shortage messages; that is the probe working, not a fault. Read its detail text in the health panel.
* **Third run (1 to 2 November 2026): dashboard time versus stored claims.** After PR #42 the dashboard took 0.003 s on an empty database, 0.6 s at about 2,300 claims and about 0.3 s at 9,160 (runs `j`, `n`), against 1.6 to 1.8 s before at 15,000 to 18,000. It still grows with the number of claims; running counts would remove that.
* **The server was updated by copying the new files over the old ones**, never overwriting `.env`, `deploy/Caddyfile.internal`, `deploy/rust/compose.gpu.yml` and `deploy/rust/compose.local.yml`; the API image was rebuilt (about 5.5 minutes) and the build cache pruned.
* **Rate limit:** the Ask limit (30 a minute) refused 282 questions in run `k`; `ASK_PER_MINUTE` was raised for run `l` and removed afterwards.

