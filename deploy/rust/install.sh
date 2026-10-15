#!/usr/bin/env bash
# First-time setup on the server, Rust build (Linux with Docker and the compose plugin).
#   bash deploy/rust/install.sh claims.example.org public      # a real domain: Let's Encrypt certificate
#   bash deploy/rust/install.sh 203.0.113.10 internal          # an IP address or internal name: private-CA certificate
#   HF_TOKEN=hf_... bash deploy/rust/install.sh 203.0.113.10 internal ai   # also host google/gemma-3-4b-it on this machine (vLLM) for the AI explanation drafts
#   bash deploy/rust/install.sh 203.0.113.10 internal ai docs  # also the document reader: import claims from PDFs and photographs (docs/45)
#   CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/install.sh ...    # also set up the host firewall: SSH only from that address, web 80/443 for all
#   (the model is overridable: CG_MODEL=<huggingface repository> bash deploy/rust/install.sh ... ai. Gemma is gated: accept its licence on
#    huggingface.co and pass a read token as HF_TOKEN. A vision model for the reader: CG_VLM_MODEL=<huggingface repository> ... ai docs. docs/40 section 12)
# It writes .env with freshly generated secrets (only if .env does not exist), builds and starts the stack, waits until it is healthy,
# then asks you to create the first administrator. Nothing secret is printed.
set -euo pipefail
cd "$(dirname "$0")/../.."

SITE="${1:?usage: install.sh <domain-or-address> <public|internal>}"
MODE="${2:?usage: install.sh <domain-or-address> <public|internal>}"
AI="${3:-}"
DOCS="${4:-}"
[ "$AI" = "docs" ] && { AI=""; DOCS="docs"; }
MODEL="${CG_MODEL:-google/gemma-3-4b-it}"
case "$MODE" in public) CADDYFILE=Caddyfile.public ;; internal) CADDYFILE=Caddyfile.internal ;; *) echo "mode must be public or internal"; exit 2 ;; esac
command -v docker >/dev/null || { echo "docker is not installed"; exit 1; }
docker compose version >/dev/null || { echo "the docker compose plugin is missing"; exit 1; }

secret() { openssl rand -base64 48 | tr -d '\n=' | tr '+/' '-_'; }
if [ -e .env ]; then
  echo ".env already exists: keeping it (delete it to generate new secrets, which would lock out existing users)"
else
  umask 077
  VLLM_KEY="$(secret)"
  {
    echo "CG_SITE=$SITE"
    echo "CADDYFILE=$CADDYFILE"
    echo "JWT_SECRET=$(secret)"
    echo "AUDIT_ANCHOR_KEY=$(secret)"
    echo "PII_KEY=$(secret)"
    echo "FERNET_KEY=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '\n')"
    echo "DATA_KEY=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '\n')"      # encrypts the claim data in the database; rotate it with `cg rotate-key`
    echo "LEDGER_SIGNING_KEY=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '\n=')"   # signs the audit-log checkpoints; the worker only (docs/45)
    echo "LEDGER_PUBLIC_KEY="                                                                    # filled in below from the seed: this one is public
    echo "MONGO_ROOT_USER=claimguard"
    echo "MONGO_ROOT_PASSWORD=$(openssl rand -hex 24)"
    echo "REDIS_PASSWORD=$(openssl rand -hex 24)"
    if [ "$AI" = "ai" ]; then
      echo "QUEUE_AI_PROVIDER=vllm"
      echo "VLLM_MODEL=$MODEL"
      echo "VLLM_API_KEY=$VLLM_KEY"        # the model server refuses a request without it
    fi
    if [ "$DOCS" = "docs" ]; then
      echo "DOCUMENT_READER_URL=http://reader:8090"
      echo "DOCUMENT_READER_TOKEN=$(secret)"
      echo "DOCUMENT_READER_ENGINE=auto"
      echo "DOCUMENT_READER_LANGS=eng"
      if [ -n "${CG_VLM_MODEL:-}" ]; then   # the second reader: a vision model served by its own vLLM (docs/45); without it documents are read with Tesseract only
        echo "VLLM_VLM_MODEL=$CG_VLM_MODEL"
        echo "VLM_BASE_URL=http://vllm-vision:8000/v1"
        echo "VLM_MODEL=$CG_VLM_MODEL"
        [ "$AI" = "ai" ] || echo "VLLM_API_KEY=$VLLM_KEY"
      fi
    fi
  } > .env
  echo "wrote .env (readable by you only). Back it up somewhere safe: without these secrets the data cannot be read."
fi

COMPOSE="docker compose -f deploy/rust/compose.yml --env-file .env"
# GPU for the model server: only when the driver and the container toolkit are both there
if command -v nvidia-smi >/dev/null && nvidia-smi >/dev/null 2>&1 && docker info 2>/dev/null | grep -qi nvidia; then
  COMPOSE="docker compose -f deploy/rust/compose.yml -f deploy/rust/compose.gpu.yml --env-file .env"
  echo "GPU found: the model server will use it."
else
  { [ "$AI" = "ai" ] || [ "$DOCS" = "docs" ]; } && echo "WARNING: no NVIDIA GPU / container toolkit detected: the models will run on the CPU (slow). Install nvidia-container-toolkit, then re-run."
fi
VLM_IN_ENV="$(grep '^VLLM_VLM_MODEL=' .env 2>/dev/null | cut -d= -f2-)"
if [ "$DOCS" = "docs" ]; then
  COMPOSE="$COMPOSE --profile documents"
  [ -n "$VLM_IN_ENV" ] && COMPOSE="$COMPOSE --profile vision"   # the vision reader has its own vLLM server (one model per server)
  grep -q '^DOCUMENT_READER_URL=.' .env || { echo "documents requested but .env has no DOCUMENT_READER_URL / DOCUMENT_READER_TOKEN: add them (see deploy/env.production.example), then re-run"; exit 1; }
fi
if [ "$AI" = "ai" ]; then
  COMPOSE="$COMPOSE --profile ai"
  grep -q '^QUEUE_AI_PROVIDER=vllm' .env || { echo "AI requested but .env has no QUEUE_AI_PROVIDER=vllm: add it with VLLM_MODEL=$MODEL and VLLM_API_KEY, then re-run"; exit 1; }
fi

# The model weights are downloaded once, before the model servers start: the servers run offline on networks with no route out, and this
# one-shot container is the only one with internet access. A gated model (Gemma) needs HF_TOKEN.
if [ "$AI" = "ai" ] || [ -n "$VLM_IN_ENV" ]; then
  TO_FETCH=""
  [ "$AI" = "ai" ] && TO_FETCH="$(grep '^VLLM_MODEL=' .env | cut -d= -f2-)"
  TO_FETCH="$TO_FETCH $VLM_IN_ENV"
  echo "downloading the model weights ($TO_FETCH): several GB, once; the only step that needs the internet..."
  CG_FETCH_MODELS="$TO_FETCH" HF_TOKEN="${HF_TOKEN:-}" $COMPOSE --profile ai-fetch run --rm vllm-fetch \
    || { echo "the model download failed (a gated model such as Gemma needs HF_TOKEN and an accepted licence: docs/40 section 12). Nothing was started."; exit 1; }
fi

$COMPOSE up -d --build
echo "waiting for the API to become healthy..."
for _ in $(seq 1 60); do
  state="$($COMPOSE ps --format '{{.Service}} {{.Health}}' api 2>/dev/null | awk '{print $2}')"
  [ "$state" = "healthy" ] && break
  sleep 3
done
[ "$state" = "healthy" ] || { echo "the API did not become healthy: $COMPOSE logs api"; exit 1; }

wait_healthy() {   # wait_healthy <service>: the model servers load their weights for several minutes
  for _ in $(seq 1 120); do
    h="$($COMPOSE ps --format '{{.Service}} {{.Health}}' "$1" 2>/dev/null | awk '{print $2}')"
    [ "$h" = "healthy" ] && return 0
    sleep 10
  done
  return 1
}

if [ "$AI" = "ai" ]; then
  echo "waiting for the model server (loading the weights and preparing the GPU takes a few minutes)..."
  wait_healthy vllm || { echo "the model server did not become healthy: $COMPOSE logs vllm   (out of memory on the card? lower VLLM_GPU_UTIL or VLLM_MAX_LEN in .env)"; exit 1; }
  echo "checking that the model answers in the required shape (about 3 minutes)..."
  $COMPOSE exec -T worker cg model-check || echo "WARNING: not every rule produced an accepted draft; claims whose draft is rejected keep the engine's own explanation. Details above."
fi

if [ "$DOCS" = "docs" ] && [ -n "$VLM_IN_ENV" ]; then
  echo "waiting for the vision model server..."
  wait_healthy vllm-vision || { echo "the vision model server did not become healthy: $COMPOSE logs vllm-vision"; exit 1; }
  echo "checking that the reader sees the vision model..."
  $COMPOSE exec -T reader python -c "import os,json,urllib.request as u; h=json.load(u.urlopen(u.Request('http://127.0.0.1:8090/health', headers={'Authorization': 'Bearer ' + os.environ['OCR_TOKEN']}))); print(h); assert h.get('vlm'), 'vision model not configured'"     || echo "WARNING: the reader did not confirm the vision model; documents will be read with Tesseract only."
fi

if grep -q '^LEDGER_SIGNING_KEY=.' .env && ! grep -q '^LEDGER_PUBLIC_KEY=.' .env; then
  PUB="$($COMPOSE run --rm --no-deps worker cg ledger pubkey 2>/dev/null | tail -n1)"
  if [ -n "$PUB" ]; then
    sed -i "s|^LEDGER_PUBLIC_KEY=.*|LEDGER_PUBLIC_KEY=$PUB|" .env
    $COMPOSE up -d api >/dev/null
    echo "Signed ledger on. Public key (copy it somewhere offline; an auditor needs only this): $PUB"
  fi
fi

if [ -n "${CG_SSH_FROM:-}" ]; then
  echo "applying the host firewall (SSH only from $CG_SSH_FROM; web ports 80 and 443 open)..."
  SUDO=""; [ "$(id -u)" = 0 ] || SUDO="sudo -E"
  $SUDO bash deploy/rust/firewall.sh apply && $SUDO bash deploy/rust/firewall.sh persist
else
  echo "NOTE: no host firewall was set up (CG_SSH_FROM is not set). Docker-published ports bypass ufw; see docs/40 section 10 and deploy/rust/firewall.sh."
fi

echo
echo "Create the first administrator (level 4). You choose the password; the authenticator seed is shown once."
$COMPOSE run --rm -it api cg admin create-admin --badge "${CG_ADMIN_BADGE:-CG-4001}" --name "${CG_ADMIN_NAME:-Administrator}"
echo
echo "Open https://$SITE/ and sign in. Then: Users (add reviewers), Routing settings (put them on shift), Submit claims."
