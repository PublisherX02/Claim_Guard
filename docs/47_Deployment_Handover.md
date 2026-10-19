# 47. Deployment hand-over (target server)

Server: Linux, NVIDIA RTX 3080 10 GB, 12 GB RAM, 8 cores. Needs: Docker + compose plugin, NVIDIA driver, `nvidia-container-toolkit`, internet access during install (model downloads, about 6 GB).

## Steps
1. `git clone -b rust-rewrite https://github.com/PublisherX02/Claim_Guard.git && cd Claim_Guard`
2. Check the GPU is visible to Docker: `docker run --rm --gpus all ubuntu nvidia-smi` (must print the card; if not, install the container toolkit and `sudo systemctl restart docker`).
3. `CG_SSH_FROM=<admin-pc-ip>/32 bash deploy/rust/install.sh <server-ip-or-domain> internal ai docs`
   (`public` instead of `internal` for a real domain with Let's Encrypt.) It: writes `.env` with fresh secrets, uses the GPU automatically, builds and starts the stack, pulls `gemma3:4b` and the vision reader `richardyoung/unlimited-ocr`, checks both, sets up the signed ledger and the firewall, then asks for the first administrator (badge CG-4001; the authenticator seed is shown once).
4. Open `https://<site>/`, sign in, add reviewers (Users), put them on shift (Routing settings), submit claims or import a PDF/photo.
5. Back up `.env` (without it the encrypted data is unreadable) and copy the ledger public key printed at the end somewhere offline.

## Checks
- `docker compose -f deploy/rust/compose.yml -f deploy/rust/compose.gpu.yml --env-file .env --profile ai --profile documents ps` : every service healthy.
- `nvidia-smi` : the card's memory in use matches `VLLM_GPU_UTIL` (and `VLLM_VLM_GPU_UTIL` if the vision server is on); `docker compose ... exec worker cg model-check` : all 15 rules accepted. No vLLM server has been started in this build: this is the first real run (`docs/40` section 12).
- If the reader warns about the vision model: documents still work with Tesseract only.

## Known limits
- The vision model is a third-party GGUF conversion (`richardyoung/unlimited-ocr`); convert the official weights and record hashes before production (docs/45). Arabic, handwriting and real hospital paper are untested.
- Throughput of 200 claims/min is unmeasured; the load test was deferred.
- The explanation model never decides a verdict; if it is down, claims keep the engine's own explanation.
