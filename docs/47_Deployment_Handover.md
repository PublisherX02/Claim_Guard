# 47. Deployment hand-over (target server)

For whoever installs ClaimGuard AI v1.0.0 on the server. This page is the short path; the reasons and the measurements are in [docs/40](40_Rust_Operations_Runbook.md) (operations, section 12 for the model server), [docs/52](52_Semantic_Rulebook_Search_Experiment.md) and [docs/55](55_Arabic_Claims_and_Field_Dictionary.md) (search by meaning), [docs/45](45_Document_Intake_Ledger_and_Streaming.md) (documents and the signed ledger) and [docs/64](64_Final_Release_and_Audit.md) (what was and was not tested).

**Target server:** Linux, NVIDIA RTX 3080 10 GB, 12 GB RAM, 8 cores. 12 GB is enough only if the optional parts are chosen with care (see the memory table below). The server needs Docker with the compose plugin, the NVIDIA driver and `nvidia-container-toolkit` (only for the `ai` and `docs` options), and internet access during the install (model downloads and image builds).

## What runs

The core is always on: the TLS proxy (Caddy), the API with the console, the queue worker, MongoDB and Redis. Everything else is an option of `install.sh`, and each option is a compose profile that can be switched on later by running the script again.

| Option | Profile | Container | What it adds | Memory limit | Disk |
|---|---|---|---|---|---|
| (none) | | `proxy`, `api`, `worker`, `mongo`, `redis` | claims, rules, review console, audit, signed ledger; the keyword search answers "Ask the rulebook" | limits add up to about 4 GB | small |
| `search` | `search` | `embed` | "Ask the rulebook" by meaning: BAAI/bge-m3 on the CPU, weights baked into the image at a pinned revision | 4 GB | image about 2.3 GB |
| `docs` | `documents` | `reader` | claims from PDFs and photos: PDFium, Tesseract (English and Arabic) | 2 GB | small |
| `docs` with `CG_VLM_MODEL` | `vision` | `vllm-vision` | a vision model for the reader | 8 GB host, share of the card | several GB |
| `ai` | `ai` | `vllm` | explanation drafts and written answers from `google/gemma-3-4b-it` served by vLLM | 8 GB host, 55% of the card | several GB |

The options can be combined in any order. On a 12 GB machine, `search` plus `docs` plus `ai` is the largest combination that fits; add `vision` only after measuring.

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

## Known limits
- **Not run on the build PC (so first runs on this server):** the `embed` image build, the production deployment itself, and a real vLLM model. docs/64 section 4 lists them. If an image fails to build, send the last 40 lines of `docker compose ... build <service>`.
- The pen test, fuzz and mutation results (docs/46) are from earlier builds of the same server and were not re-run for this release.
- The vision model is whatever Hugging Face repository `CG_VLM_MODEL` names; it is not shipped or pinned by hash. Record the hashes of the weights you choose before production (docs/45). Arabic, handwriting and real hospital paper are untested.
- Throughput of 200 claims/min is unmeasured; the load test was deferred.
- The explanation model never decides a verdict; if it is down, claims keep the engine's own explanation.
- Everything in the system is synthetic data. A passed pre-check is not a payer approval.
