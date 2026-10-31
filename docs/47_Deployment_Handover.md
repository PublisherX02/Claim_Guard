# 47. Deployment hand-over (target server)

For whoever installs ClaimGuard AI v1.0.0 on the server. This page is the short path; the reasons and the measurements are in [docs/40](40_Rust_Operations_Runbook.md) (operations, section 12 for the model server), [docs/52](52_Semantic_Rulebook_Search_Experiment.md) and [docs/55](55_Arabic_Claims_and_Field_Dictionary.md) (search by meaning), [docs/45](45_Document_Intake_Ledger_and_Streaming.md) (documents and the signed ledger) and [docs/64](64_Final_Release_and_Audit.md) (what was and was not tested).

**Target server:** Linux, NVIDIA RTX 3080 10 GB, 12 GB RAM, 8 cores. 12 GB is enough only if the optional parts are chosen with care (see the memory table below). The server needs Docker with the compose plugin, the NVIDIA driver and `nvidia-container-toolkit` (only for the `ai` and `docs` options), and internet access during the install (model downloads and image builds). **Disk: at least 100 GB** (a 60 GB disk ran full while pulling the images and was enlarged to 99 GB). The install script needs `openssl`, `awk`, `sed`, `grep`, `base64` and `df`; a minimal image may lack them (`sudo apt-get install -y curl openssl gawk`).

## What runs

The core is always on: the TLS proxy (Caddy), the API with the console, the queue worker, MongoDB and Redis. Everything else is an option of `install.sh`, and each option is a compose profile that can be switched on later by running the script again.

| Option | Profile | Container | What it adds | Memory limit | Disk |
|---|---|---|---|---|---|
| (none) | | `proxy`, `api`, `worker`, `mongo`, `redis` | claims, rules, review console, audit, signed ledger; the keyword search answers "Ask the rulebook" | limits add up to about 4 GB | small |
| `search` | `search` | `embed` | "Ask the rulebook" by meaning: BAAI/bge-m3 on the CPU, weights baked into the image at a pinned revision | 4 GB | image about 2.3 GB |
| `docs` | `documents` | `reader` | claims from PDFs and photos: PDFium, Tesseract (English and Arabic) | 2 GB | small |
| `docs` with `CG_VLM_MODEL` | `vision` | `vllm-vision` | a vision model for the reader (read phone photos better; see "The vision model" below) | 8 GB host, share of the card | several GB |
| `ai` | `ai` | `vllm` | explanation drafts and written answers from `google/gemma-3-4b-it` served by vLLM | 8 GB host, 85% of the card (measured peak 7.7 GiB of 10) | several GB |

The options can be combined in any order. On a 12 GB machine, `search` plus `docs` plus `ai` is the largest combination that fits (measured: the whole stack peaked at 8.9 GiB, 75 % of the memory, docs/66); `vision` does not fit beside `ai` on a 10 GB card.

## Steps
1. `git clone https://github.com/PublisherX02/Claim_Guard.git && cd Claim_Guard` (the final code is on `main`).
2. Only for `ai` or `docs`: check the GPU is visible to Docker: `docker run --rm --gpus all ubuntu nvidia-smi` (it must print the card; if not, install the container toolkit and `sudo systemctl restart docker`). Gemma is gated: accept its licence on huggingface.co with the account that owns the token, and export `HF_TOKEN=<read token>`.
3. Run the installer with the site name and the options you want:
   - Core only, internal certificate: `CG_SSH_FROM=<admin-pc-ip>/32 bash deploy/rust/install.sh <server-ip-or-name> internal`
   - Everything: `HF_TOKEN=hf_... CG_SSH_FROM=<admin-pc-ip>/32 bash deploy/rust/install.sh <server-ip-or-name> internal ai docs search`
   - A real domain with a Let's Encrypt certificate: use `public` instead of `internal`.

   The script writes `.env` with fresh secrets (only if `.env` does not exist), uses the GPU automatically, downloads the model weights once through the one container that has internet access, builds and starts the stack, waits until the API is healthy, runs `cg model-check` when `ai` is on, sets up the signed ledger and the host firewall, then asks for the first administrator (badge `CG-4001`; the authenticator seed is shown once).
4. Open `https://<site>/`, sign in, add reviewers (Users), put them on shift (Routing settings), submit claims or import a PDF or photo.
5. Back up `.env` (without it the encrypted data is unreadable) and copy the ledger public key printed at the end somewhere offline. `deploy/rust/backup.sh` and `restore.sh` cover the data; docs/40 explains the schedule.

## Switching an option on later
Re-run the same command with the extra option. `.env` is kept, so the secrets do not change. If `.env` already exists but lacks the lines an option needs, the script says which ones (for `search`: `EMBEDDER_URL=http://embed:8091`, `EMBEDDER_TOKEN`, `EMBEDDER_THRESHOLD=0.485`; for `docs`: `DOCUMENT_READER_URL`, `DOCUMENT_READER_TOKEN`; for `ai`: `QUEUE_AI_PROVIDER=vllm`, `VLLM_MODEL`, `VLLM_API_KEY`) and stops before changing anything.

## Checks
- `docker compose -f deploy/rust/compose.yml --env-file .env ps` (add `-f deploy/rust/compose.gpu.yml` when the GPU file is used, and one `--profile` per option you installed): every service healthy.
- Search by meaning: the `embed` container is healthy, and in the console "Ask the rulebook" answers a question with a citation. The refusal threshold is `EMBEDDER_THRESHOLD=0.485`, measured for bge-m3; the same weights give identical vectors on the stack in this release (transformers 5.10.4, torch 2.14.1) and on the one before it. If `embed` is down, the keyword search answers on its own.
- Documents: the `reader` container is healthy; import a PDF or a photo in the console. With a vision model, `install.sh` also confirms that the reader sees it. If the reader warns about the vision model, documents still work with Tesseract only.
- Model server (`ai`): `nvidia-smi` shows the card's memory in use matching `VLLM_GPU_UTIL` (and `VLLM_VLM_GPU_UTIL` when the vision server is on); `docker compose ... exec worker cg model-check` accepts all 15 rules.
- Console: sign in as the administrator, open System health (every check green), Audit (the signed ledger verifies), then take a claim as a reviewer.
- Measurements: once the stack is healthy and before real data goes in, run the server experiments (`experiments/server/run_all.py`, plan in [docs/65](65_Server_Experiment_Plan.md)). They measure the model server, the pipeline, memory, search, documents, restarts and exposure, and write the results chapter [docs/66](66_Server_Experiment_Results.md).

## The vision model (reading phone photos)
Tesseract alone read digital PDFs at 100 % of the fields, scans at 98 % and image-only PDFs at 97 %, but phone photos at **50 %** (docs/66, S4). A vision model as second reader lifted photos to 88.1 % on the build PC (`baidu/Unlimited-OCR`, 3.3B parameters, MIT licence, served by Ollama; docs/45). **The server run did not include it: the model is never part of the install unless you ask for it** (`CG_VLM_MODEL` is empty by default, so only Tesseract runs).

To switch it on: `CG_VLM_MODEL=baidu/Unlimited-OCR bash deploy/rust/install.sh <site> internal docs` (add `ai search` for the rest). Three cautions, none of them yet measured on the server:
- **Memory.** The explanation model holds 85 % of a 10 GB card. A 3.3B vision model needs several GB more, so the two do not fit side by side; `install.sh` refuses a sum of shares above 0.95. Choose: documents with the vision model and no explanation model, a card with more memory, or another approach (image clean-up before Tesseract, a smaller vision model).
- **vLLM support.** The 88.1 % was measured through Ollama, where the model returned fused table cells and could loop (docs/45). Whether vLLM serves this architecture has not been tried: if `vllm-vision` does not become healthy, read `docker compose ... logs vllm-vision` and tell us.
- **Pin the weights.** Use `CG_FETCH_MODELS="google/gemma-3-4b-it baidu/Unlimited-OCR@<commit hash>"` with the download step so the same files are used every time.
After the change, run S4 again on the server (docs/65) and check S0.

## GPU in a container (Proxmox / LXC)
The target server is a Proxmox container, where a GPU does not work out of the box. These are the problems the first install met and how each was solved (from the server report, docs/66); they were fixed by hand on that machine and **the override file itself is not in the repository yet**. Put such changes in `deploy/rust/compose.local.yml` (see the next section), not in `compose.gpu.yml`.
| Symptom | Cause | What solved it |
|---|---|---|
| GPU not usable in Docker (BPF error) | the container blocks cgroup control | run the NVIDIA container toolkit in no-cgroups mode |
| GPU libraries missing inside containers | versioned library files absent | copy the `.so.1` files to versioned names and use CDI device access |
| vLLM would not start (missing CUDA 13 libraries) | the driver (615.x) is newer than the image's CUDA 12.9 build | mount the host's `/opt/nvrtc13` and set `LD_LIBRARY_PATH` |
| vLLM compile error | `/tmp` mounted `noexec` | fixed in this version (`/tmp:size=4g,exec` in `compose.yml`) |
| vLLM: no memory for the KV cache | too small a share of the card | `VLLM_GPU_UTIL=0.85`, now the default |
| Model download refused (403) | Gemma is gated | accept its licence on huggingface.co, pass the token as `HF_TOKEN`, revoke it afterwards |

## Keeping local changes across updates
An update that replaces the repository files must not overwrite what is specific to your machine. Keep it in files that are not in the repository (both are in `.gitignore`):
- `.env` (secrets, model names, memory shares);
- `deploy/rust/compose.local.yml`: extra compose settings such as GPU mounts or devices; `install.sh` adds it automatically when it exists (add `-f deploy/rust/compose.local.yml` yourself when you call `docker compose` by hand);
- `deploy/Caddyfile.local`: your own proxy file, selected with `CADDYFILE=Caddyfile.local` in `.env`.
Before an update, back up the database and those three files (`deploy/rust/backup.sh`), copy the new files over, and compare.

## Behind Cloudflare
The server of the first install sits behind Cloudflare. Points that the first run showed, or that follow from it:
- **A test through Cloudflare tests Cloudflare.** The port scan, the TLS versions and the certificate seen from outside are Cloudflare's (it also answers on port 8080, which is not an open port of the server). Check the server itself from another network, using its own address, and look at what listens with `sudo ss -tlnp`.
- **Do not leave the origin open to everyone.** Allow ports 80 and 443 only from Cloudflare's published address ranges (cloudflare.com/ips) in the host or Proxmox firewall, or use a Cloudflare Tunnel; otherwise anyone who learns the server's address can skip Cloudflare. `deploy/rust/firewall.sh` does not do this yet.
- **Encryption between Cloudflare and the server.** `Caddyfile.internal` uses Caddy's own private certificate (`tls internal`), which Cloudflare's "Full (strict)" mode refuses; "Full" accepts it but does not check it. For strict mode install a free Cloudflare Origin CA certificate in the proxy.
- **HTTP to HTTPS.** The first install needed a Cloudflare rule (it had a typo at first); turn on "Always Use HTTPS" in Cloudflare and check that port 80 of the server never serves the application.
- **The client address.** The API takes the client from the last `X-Forwarded-For` entry that the proxy adds; behind Cloudflare that is a Cloudflare address, so per-client limits and the audit log see the same few addresses. The proxy can be told to trust Cloudflare and read `CF-Connecting-IP` (Caddy `servers { trusted_proxies static <Cloudflare ranges>  client_ip_headers CF-Connecting-IP }`); this is **untested here**, so try it on a copy first.
- **Cloudflare sees the traffic in clear text**, including claim data. Decide that on purpose and write it in the privacy note.
- **A sign-in error "That request came from another site"** appeared when the API saw a different `Host` than the browser's `Origin`; the first install fixed it with `header_up Host <the public domain>` in each proxy block. Caddy passes the original `Host` through by default, so look first at what name Cloudflare and the site address use.
- **Test scripts** (`experiments/server`) now send their own User-Agent, because Cloudflare refuses the default Python one with error 1010.

## Known limits
- **First install on the server (docs/66):** the `embed` image, the production deployment and a real vLLM model ran there for the first time and ran. The first run found problems that are fixed in this version: the embedding service could not find its model offline (`refs/main` was missing from a hash-pinned download), vLLM could not compile into a `noexec` `/tmp`, the API and worker memory limit was too low, and the explanation model needed 0.85 of the card, not 0.55. If an image fails to build, send the last 40 lines of `docker compose ... build <service>`.
- The pen test, fuzz and mutation results (docs/46) are from earlier builds of the same server and were not re-run for this release.
- The vision model is whatever Hugging Face repository `CG_VLM_MODEL` names; it is not shipped or pinned by hash. Record the hashes of the weights you choose before production (docs/45). Arabic, handwriting and real hospital paper are untested.
- Throughput was measured on the target server (docs/66, run `server_20261031`): 376 explanations a minute and 11,721 claims a minute into the queue, both far above the 200 target. The claims figure is intake into the queue, not claims decided: reviewer time is the real limit (docs/49).
- The explanation model never decides a verdict; if it is down, claims keep the engine's own explanation.
- Everything in the system is synthetic data. A passed pre-check is not a payer approval.
