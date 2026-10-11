#!/usr/bin/env bash
# First-time setup on the server (Linux with Docker and the compose plugin).
#   bash deploy/install.sh claims.example.org public      # a real domain: Let's Encrypt certificate
#   bash deploy/install.sh 203.0.113.10 internal          # an IP address or internal name: private-CA certificate
#   bash deploy/install.sh 203.0.113.10 internal ai       # also host gemma3:4b on this machine (Ollama) for the AI explanation drafts
#   (the model is overridable: CG_MODEL=gemma3:12b bash deploy/install.sh ... ai)
# It writes .env with freshly generated secrets (only if .env does not exist), builds and starts the stack, waits until it is healthy,
# then asks you to create the first administrator. Nothing secret is printed.
set -euo pipefail
cd "$(dirname "$0")/.."

SITE="${1:?usage: install.sh <domain-or-address> <public|internal>}"
MODE="${2:?usage: install.sh <domain-or-address> <public|internal>}"
AI="${3:-}"
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
    echo "MONGO_ROOT_USER=claimguard"
    echo "MONGO_ROOT_PASSWORD=$(openssl rand -hex 24)"
    if [ "$AI" = "ai" ]; then
      echo "QUEUE_AI_PROVIDER=ollama"
      echo "OLLAMA_MODEL=$MODEL"
    fi
  } > .env
  echo "wrote .env (readable by you only). Back it up somewhere safe: without these secrets the data cannot be read."
fi

COMPOSE="docker compose -f deploy/compose.yml --env-file .env"
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
  $COMPOSE exec -T worker python scripts/check_queue_model.py || echo "WARNING: not every rule produced an accepted draft; claims whose draft is rejected keep the engine's own explanation. Details above."
fi

echo
echo "Create the first administrator (level 4). You choose the password; the authenticator seed is shown once."
$COMPOSE run --rm -it api python scripts/access_admin.py create-admin --badge "${CG_ADMIN_BADGE:-CG-4001}" --name "${CG_ADMIN_NAME:-Administrator}"
echo
echo "Open https://$SITE/ and sign in. Then: Users (add reviewers), Routing settings (put them on shift), Submit claims."
