#!/usr/bin/env bash
# Restore an encrypted backup made by backup.sh into the running Rust stack (the stack must be up; the API and worker are stopped during
# the restore and started again).  BACKUP_KEY_FILE=... bash deploy/rust/restore.sh /srv/backups/claimguard-20261010.tar.enc
# This REPLACES the database contents and the log volumes with the backup's. Use the same .env secrets the backup was made under.
set -euo pipefail
cd "$(dirname "$0")/../.."
IN="${1:?usage: BACKUP_KEY_FILE=<file> restore.sh <backup file>}"
KEY="${BACKUP_KEY_FILE:?set BACKUP_KEY_FILE to a file holding the backup passphrase}"
[ -s "$KEY" ] || { echo "the key file is missing or empty"; exit 2; }
if [ -e "$IN.sha256" ]; then ( cd "$(dirname "$IN")" && sha256sum -c "$(basename "$IN").sha256" ) || { echo "the backup file does not match its checksum"; exit 1; }; fi
PROJECT="${COMPOSE_PROJECT:-claimguard}"
ENVFILE="${ENV_FILE:-.env}"
COMPOSE="docker compose -f deploy/rust/compose.yml --env-file $ENVFILE -p $PROJECT"
umask 077
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
HOSTTMP="$TMP"; command -v cygpath >/dev/null 2>&1 && HOSTTMP="$(cygpath -w "$TMP")"
openssl enc -d -aes-256-cbc -pbkdf2 -iter 600000 -pass "file:$KEY" -in "$IN" -out "$TMP/payload.tar" || { echo "wrong key, or the file is damaged"; exit 1; }
( cd "$TMP" && tar xf payload.tar && sha256sum -c SHA256SUMS ) || { echo "the contents do not match their checksums"; exit 1; }
$COMPOSE stop api worker
for archive in "$TMP"/mongo-*.archive.gz; do
  name="$(basename "$archive" .archive.gz)"; name="${name#mongo-}"
  out="$($COMPOSE exec -T mongo sh -c "mongorestore --archive --gzip --drop --nsInclude '$name.*' -u \"\$MONGO_INITDB_ROOT_USERNAME\" -p \"\$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin" < "$archive" 2>&1)" || { echo "$out" | tail -5; echo "restoring $name failed: the database may be half restored, run this again"; exit 1; }
  echo "$out" | grep -q " 0 document(s) failed to restore" || { echo "$out" | tail -5; echo "some documents of $name did not restore"; exit 1; }
  echo "$name: $(echo "$out" | grep -o '[0-9]* document(s) restored successfully')"
done
MSYS_NO_PATHCONV=1 docker run --rm -v "${PROJECT}_claimguard_data:/data" -v "${PROJECT}_audit_mirror:/mirror" -v "$HOSTTMP":/in:ro alpine sh -c 'rm -rf /data/* /data/.[!.]* /mirror/* 2>/dev/null; tar xzf /in/logs.tgz -C / && chown -R 65532:65532 /data /mirror'
$COMPOSE start api worker
echo "restored from $IN; check the health panel and the audit verification before using the system"
