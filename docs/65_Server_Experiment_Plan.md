# 65. Server experiment plan

What to measure on the target server, how, with which scripts, and what counts as a pass. The scripts are in `experiments/server/`; their output becomes the results chapter ([docs/66](66_Server_Experiment_Results.md)) and the figures in `docs/figures/server/`. Everything on the build PC so far was measured on a laptop with no GPU and no Docker images (docs/64 section 4); these experiments close that gap.

**Target server:** Linux, NVIDIA RTX 3080 10 GB, 12 GB RAM, 8 cores, installed with `deploy/rust/install.sh` and the options `ai docs search` (docs/47).

## 1. Questions the experiments answer

| Id | Question | Why it matters |
|---|---|---|
| S0 | How much memory, GPU and CPU does the whole stack use, and does it grow? | 12 GB is tight: the limits of docs/47 add up to more than the machine has, so the real peak must be measured. |
| S1 | Does the model server reach the design's 200 explanations a minute on one RTX 3080, and how much of the card does it take? | The 200 a minute is a design assumption (docs/40), never measured. |
| S2 | How many claims a minute does the whole pipeline take in, in batches of 10 to 500, and does a long steady load degrade it? | The same 200 a minute, but through the proxy, API, queue, engine and store. |
| S3 | Does "Ask the rulebook" by meaning on the server give the quality measured in docs/52, and how fast is it? | The embedding image has never been built here; the 0.485 refusal threshold depends on identical vectors. |
| S4 | How long does a document take, and how accurate is the reading on the server's reader? | docs/45 numbers are from the build PC. |
| S5 | When a service restarts while claims arrive, is anything acknowledged lost, and how long is the server unavailable? | The reliability claim of the design (docs/40, docs/53). |
| S6 | What does the server show to the network? | Only 80 and 443 should answer; internal ports must be closed. |
| S7 | Do the health panel, the audit chains and the signed ledger verify on the live stack, and does a real backup work and stay encrypted? | The integrity claims of docs/58 and docs/59, on the deployed system. |

## 2. Method and pass criteria

| Id | Script | What it does | Target (and reason) | About |
|---|---|---|---|---|
| S0 | `sampler.py` | Every 5 s: `nvidia-smi`, `docker stats`, `/proc/meminfo`, for the whole run | Peak of all containers at most 85 % of server memory; GPU at most 95 % of the card; no container above 95 % of its limit; memory growth under 100 MiB an hour | whole run |
| S1 | `s01_model.py` (runs `rust/tools/model_bench.py` on the compose network) | Concurrency 1 to 64, at least 16 requests per level, one explanation per request, temperature 0 | At least 200 a minute at some level, no failed request (the design's rate) | 10 to 20 min |
| S2 | `s02_pipeline.py` | Batches of 10, 50, 200, 500 claims through `POST /api/v1/queue/submit` (asynchronous), then three read requests are timed before and after; optional soak at a steady rate | At least 200 claims a minute at some batch size, no failed batch, the 95th percentile of each read request under 2 s right after the load | 15 min, plus the soak (30 to 60 min) |
| S3 | `s03_search.py` | The 26 frozen questions and 5 off-topic strings of `experiments/rag_semantic/`, then the same at concurrency 1 to 8 | At least 90 % of questions return the right rule; every off-topic string is refused; no 429 | 5 min |
| S4 | `s04_documents.py` | Documents made by `rust/tools/make_docs.py` go through the Import route; fields compared with the answer key | At least 95 % of fields exactly right; no document refused | 20 min |
| S5 | `s05_resilience.py` | While a client submits, each of worker, api, redis, mongo, proxy is restarted; every acknowledged claim is read back | Zero acknowledged claims missing; every service back within 5 minutes | 15 min (interrupts the server) |
| S6 | `s06_exposure.py` (from another machine) | Port check, TLS versions, headers, anonymous requests | Only 80 and 443 (and SSH) open; TLS 1.0 and 1.1 refused; HSTS, CSP, no server version; anonymous API and health panel refused | 2 min |
| S7 | `s07_integrity.py` | Health panel, audit verification, ledger, and with a key file a real `backup.sh` | No failing probe; chains and ledger verify; backup file encrypted with a matching SHA-256 | 5 min |

The targets are not tuned to what is expected: where a measure can be argued either way, the stricter number is used. A FAIL is a finding. Write what was changed and run that experiment again; keep both runs (each run has its own folder).

## 3. Preparation (once)

1. Install as in docs/47 with `ai docs search`. Do this on a stack that holds **no real data**: S2 and S5 put thousands of synthetic claims into the database and the reviewers' queue, and S5 interrupts the service. The best moment is after install and before go-live.
2. Create two benchmark accounts in the console (Users): an **administrator** (submits claims, imports documents, reads the health panel) and a **viewer** at level 1 (reads claims and asks the rulebook; the administrator may not). For each, note the badge, the password and the **authenticator seed** (shown once; the base32 text under the QR code). Delete both accounts when the experiments are done.
3. For S3, set `ASK_PER_MINUTE=100000` in `.env` and `docker compose ... up -d api`; put the old value back afterwards. If the limit still bites, the report counts the 429 answers.
4. For S4, make the documents once on any machine with `pip install reportlab pypdfium2 pillow numpy`: `python rust/tools/make_docs.py --out /srv/docs_test --n 30`, and copy the folder to the server.
5. Export the credentials in the shell that runs the scripts (they are never put on the command line):
   `export CG_URL=https://<server> CG_BADGE=CG-4xxx CG_PASSWORD='...' CG_TOTP_SEED=<base32>`  (administrator)
   `export CG_VIEW_BADGE=CG-1xxx CG_VIEW_PASSWORD='...' CG_VIEW_TOTP_SEED=<base32>`  (viewer; needed by S3 and S5, used by S2 when set)
6. The scripts use only the Python standard library. The report needs `pip install matplotlib` (on the server or on the admin PC; the run folder can be copied).

## 4. Running

```
# on the server, from the repository root
python3 experiments/server/run_all.py --run server_$(date +%Y%m%d) \
    --docs /srv/docs_test --soak-minutes 30 --soak-rate 100 \
    --backup-key-file /root/claimguard-backup.key --backup-dir /srv/backups \
    --yes-restart                      # S5; leave it out to skip the restarts

# from the admin PC (a different machine), any time
python experiments/server/s06_exposure.py --run server_$(date +%Y%m%d) --host <server>
```

A failing experiment does not stop the others. `outputs/server/<run>/` receives one JSON file per experiment, `samples.jsonl` (S0), `env.json` (the machine) and `run.json` (what ran, how long, exit codes). To rebuild only the report: `python experiments/server/make_server_report.py --run <run>`.

The sampler asks `docker stats` every 5 s, so the container memory figures are samples, not a continuous record; a spike shorter than that can be missed. Use `--sample-every 2` for a short run.

## 5. What comes out

* `docs/66_Server_Experiment_Results.md`: the machine, a scoreboard of every measure against its target, then one section per experiment with its table and figure.
* `docs/figures/server/<run>_*.png`: S1 model throughput and latency; S2 throughput by batch size and the soak; S0 GPU over time, peak memory by container, memory of the whole stack over time; S3 quality and latency; S4 accuracy and speed; S5 unavailability per service.
* For the report: cite the scoreboard, include the figures, and put the "Limits" below next to the numbers.

Send back one file: `python experiments/server/pack_results.py --run <run>` makes `outputs/server/<run>.zip` (a few MB) and refuses if anything in the run looks like a secret. It holds no claim text and no credential: the scripts log counts, times and claim ids that were made for the test.

## 6. Cleaning up

* Delete the two benchmark accounts; restore `ASK_PER_MINUTE`.
* The synthetic claims stay in the database: there is no bulk delete by design (docs/58). On a fresh install the simplest cleanup is `docker compose ... down -v` and a new install before go-live.
* Keep the backup made by S7 only until the restore rehearsal on a second machine has been done, then delete it.

## 7. Limits of these experiments

* Synthetic data only. Real hospital paper, handwriting, stamps and Arabic documents are not covered.
* One server, one run per experiment: the percentiles are of requests inside a run; run-to-run variation is not measured. For a figure that goes into a decision, run S1 and S2 three times.
* The load generator is a single Python process. On the server itself it measures the stack without the network; from the admin PC it adds the LAN. The first is the stack's capacity, the second what a user sees. Run S2 both ways if the difference matters.
* S2 measures claims taken into the queue (checked by the engine and stored), not claims decided: reviewer time is the real bottleneck and is modelled in docs/49.
* A rehearsal on a laptop (`--dry`) proves the scripts work and is marked as such in every file; its numbers are not results.
* The 5-second sampler (S0) can miss a peak shorter than that.
* The soak is 30 to 60 minutes: it shows a leak of tens of MiB an hour, not of a few MiB a day. A 24-hour soak is the right test before a long unattended period.
* Behind Cloudflare (or any other front), S6 and the other scripts measure the front, not the server. Run S6 against the server's own address from another network too, and check what listens on the server (`ss -tlnp`).
* Some CDNs refuse the default Python User-Agent (Cloudflare: error 1010); the scripts send their own.
