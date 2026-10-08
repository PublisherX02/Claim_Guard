#!/usr/bin/env bash
# Back up MongoDB (all claims, users, queue state) and the audit-log volume into ./backup/<timestamp>/.
# Run from the repository root; schedule with cron. Keep copies off this machine. Restore: see docs/35.
set -euo pipefail
cd "$(dirname "$0")/.."
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="backup/$STAMP"
mkdir -p "$OUT"
COMPOSE="docker compose -f deploy/compose.yml --env-file .env"
set -a; . ./.env; set +a
$COMPOSE exec -T mongo mongodump --archive --gzip --username "$MONGO_ROOT_USER" --password "$MONGO_ROOT_PASSWORD" --authenticationDatabase admin > "$OUT/mongo.archive.gz"
docker run --rm -v claimguard_claimguard_data:/data:ro -v "$PWD/$OUT":/out alpine tar czf /out/audit-logs.tgz -C /data .
( cd "$OUT" && sha256sum mongo.archive.gz audit-logs.tgz > SHA256SUMS )
chmod -R go-rwx "$OUT"
echo "backup written to $OUT"
