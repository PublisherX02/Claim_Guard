#!/usr/bin/env bash
# First-time setup on the server, Rust build (Linux with Docker and the compose plugin).
#   bash deploy/rust/install.sh claims.example.org public      # a real domain: Let's Encrypt certificate
#   bash deploy/rust/install.sh 203.0.113.10 internal          # an IP address or internal name: private-CA certificate
#   bash deploy/rust/install.sh 203.0.113.10 internal ai       # also host gemma3:4b on this machine (Ollama) for the AI explanation drafts
#   bash deploy/rust/install.sh 203.0.113.10 internal ai docs  # also the document reader: import claims from PDFs and photographs (docs/45)
#   CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/install.sh ...    # also set up the host firewall: SSH only from that address, web 80/443 for all
#   (the model is overridable: CG_MODEL=gemma3:12b bash deploy/install.sh ... ai)
# It writes .env with freshly generated secrets (only if .env does not exist), builds and starts the stack, waits until it is healthy,
# then asks you to create the first administrator. Nothing secret is printed.
set -euo pipefail
cd "$(dirname "$0")/../.."

SITE="${1:?usage: install.sh <domain-or-address> <public|internal>}"
MODE="${2:?usage: install.sh <domain-or-address> <public|internal>}"
AI="${3:-}"
DOCS="${4:-}"
[ "$AI" = "docs" ] && { AI=""; DOCS="docs"; }
MODEL="${CG_MODEL:-gemma3:4b}"
case "$MODE" in public) CADDYFILE=Caddyfile.public ;; internal) CADDYFILE=Caddyfile.internal ;; *) echo "mode must be public or internal"; exit 2 ;; esac
command -v docker >/dev/null || { echo "docker is not installed"; exit 1; }
docker compose version >/dev/null || { echo "the docker compose plugin is missing"; exit 1; }

secret() { openssl rand -base64 48 | tr -d '\n=' | tr '+/' '-_'; }
if [ -e .env ]; then
  echo ".env already exists: keeping it (delete it to generate new secrets, which would lock out existing users)"
else
  umask 077
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
      echo "QUEUE_AI_PROVIDER=ollama"
      echo "OLLAMA_MODEL=$MODEL"
    fi
    if [ "$DOCS" = "docs" ]; then
      echo "DOCUMENT_READER_URL=http://reader:8090"
      echo "DOCUMENT_READER_TOKEN=$(secret)"
      echo "DOCUMENT_READER_ENGINE=auto"
      echo "DOCUMENT_READER_LANGS=eng"
      echo "VLM_BASE_URL="      # set to http://ollama:11434/v1 and VLM_MODEL to a vision model you pulled into Ollama, to add the second reader (docs/45)
      echo "VLM_MODEL="
    fi
  } > .env
  echo "wrote .env (readable by you only). Back it up somewhere safe: without these secrets the data cannot be read."
fi

COMPOSE="docker compose -f deploy/rust/compose.yml --env-file .env"
if [ "$DOCS" = "docs" ]; then
  COMPOSE="$COMPOSE --profile documents"
  grep -q '^DOCUMENT_READER_URL=.' .env || { echo "documents requested but .env has no DOCUMENT_READER_URL / DOCUMENT_READER_TOKEN: add them (see deploy/env.production.example), then re-run"; exit 1; }
fi
if [ "$AI" = "ai" ]; then
  COMPOSE="$COMPOSE --profile ai"
  grep -q '^QUEUE_AI_PROVIDER=ollama' .env || { echo "AI requested but .env has no QUEUE_AI_PROVIDER=ollama: add it with OLLAMA_MODEL=$MODEL, then re-run"; exit 1; }
fi
$COMPOSE up -d --build
echo "waiting for the API to become healthy..."
for _ in $(seq 1 60); do
  state="$($COMPOSE ps --format '{{.Service}} {{.Health}}' api 2>/dev/null | awk '{print $2}')"
  [ "$state" = "healthy" ] && break
  sleep 3
done
[ "$state" = "healthy" ] || { echo "the API did not become healthy: $COMPOSE logs api"; exit 1; }

if [ "$AI" = "ai" ]; then
  MODEL_IN_ENV="$(grep '^OLLAMA_MODEL=' .env | cut -d= -f2-)"
  echo "downloading the model $MODEL_IN_ENV into the local model server (a few GB, once)..."
  $COMPOSE exec -T ollama ollama pull "$MODEL_IN_ENV"
  echo "checking that the model answers in the required shape (about 3 minutes)..."
  $COMPOSE exec -T worker cg model-check || echo "WARNING: not every rule produced an accepted draft; claims whose draft is rejected keep the engine's own explanation. Details above."
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
