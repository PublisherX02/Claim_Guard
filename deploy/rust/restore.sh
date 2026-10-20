#!/usr/bin/env bash
# Restore an encrypted backup made by backup.sh into the running Rust stack (the stack must be up; the API and worker are stopped during
# the restore and started again).  BACKUP_KEY_FILE=... bash deploy/rust/restore.sh /srv/backups/claimguard-20261010.tar.enc
# This REPLACES the database contents and the log volumes with the backup's. Use the same .env secrets the backup was made under.
# The patient keys come from <backup>.keys (see backup.sh). After the restore the erasure ledger is replayed: every erasure made since the
# backup is applied again, so a patient who asked to be erased does not come back with the backup. The ledger itself is never rolled back:
# the live file is kept if it is longer than the one in the backup (it only ever grows).
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
KEYS_IN="$IN.keys"
if [ -e "$KEYS_IN" ]; then
  [ ! -e "$KEYS_IN.sha256" ] || ( cd "$(dirname "$KEYS_IN")" && sha256sum -c "$(basename "$KEYS_IN").sha256" ) || { echo "the keys file does not match its checksum"; exit 1; }
  openssl enc -d -aes-256-cbc -pbkdf2 -iter 600000 -pass "file:$KEY" -in "$KEYS_IN" -out "$TMP/keys.archive.gz" || { echo "wrong key, or the keys file is damaged"; exit 1; }
  cp "$TMP/keys.archive.gz" "$TMP/mongo-keys.archive.gz"
else
  echo "WARNING: $KEYS_IN is missing. The claims will restore but every patient's data is unreadable without the patient keys."
fi
$COMPOSE stop api worker
# keep the live erasure ledger: copy it aside before the volume is replaced
MSYS_NO_PATHCONV=1 docker run --rm -v "${PROJECT}_claimguard_data:/data:ro" -v "$HOSTTMP":/out alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 sh -c 'cp /data/erasure_ledger.jsonl /out/ledger.live 2>/dev/null || true'

for archive in "$TMP"/mongo-*.archive.gz; do
  name="$(basename "$archive" .archive.gz)"; name="${name#mongo-}"
  [ "$name" != "keys" ] || name="$(grep -E '^KEYS_DB=' "$ENVFILE" | cut -d= -f2- || true)"
  if [ -z "$name" ]; then QN="$(grep -E '^QUEUE_DB=' "$ENVFILE" | cut -d= -f2- || true)"; D="$(grep -E '^MONGO_DB=' "$ENVFILE" | cut -d= -f2- || true)"; D="${D:-claimguard}"; name="${QN:-${D}_queue}_keys"; fi
  out="$($COMPOSE exec -T mongo sh -c "mongorestore --archive --gzip --drop --nsInclude '$name.*' -u \"\$MONGO_INITDB_ROOT_USERNAME\" -p \"\$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin" < "$archive" 2>&1)" || { echo "$out" | tail -5; echo "restoring $name failed: the database may be half restored, run this again"; exit 1; }
  echo "$out" | grep -q " 0 document(s) failed to restore" || { echo "$out" | tail -5; echo "some documents of $name did not restore"; exit 1; }
  echo "$name: $(echo "$out" | grep -o '[0-9]* document(s) restored successfully')"
done
MSYS_NO_PATHCONV=1 docker run --rm -v "${PROJECT}_claimguard_data:/data" -v "${PROJECT}_audit_mirror:/mirror" -v "$HOSTTMP":/in:ro alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 sh -c 'rm -rf /data/* /data/.[!.]* /mirror/* 2>/dev/null; tar xzf /in/logs.tgz -C / && chown -R 65532:65532 /data /mirror'
# put the live ledger back if it is longer than the backup's (it only ever grows), then apply every erasure again before anyone is served
if [ -s "$TMP/ledger.live" ]; then
  MSYS_NO_PATHCONV=1 docker run --rm -v "${PROJECT}_claimguard_data:/data" -v "$HOSTTMP":/in:ro alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 sh -c 'if [ ! -e /data/erasure_ledger.jsonl ] || [ "$(wc -c < /in/ledger.live)" -gt "$(wc -c < /data/erasure_ledger.jsonl)" ]; then cp /in/ledger.live /data/erasure_ledger.jsonl && chown 65532:65532 /data/erasure_ledger.jsonl; fi'
fi
# no --data-dir: the image already sets CLAIMGUARD_DATA_DIR=/data (Git Bash on Windows rewrites a bare /data argument into a Windows path, and the replay then found no ledger)
REPLAY="$($COMPOSE run --rm --no-deps -T worker cg replay-erasures)" || { echo "$REPLAY"; echo "replaying the erasure ledger FAILED: do not start the system, an erased patient may have come back"; exit 1; }
echo "$REPLAY"
# fail closed: a ledger with content that the replay did not read is the one way an erased patient comes back unnoticed
if [ -s "$TMP/ledger.live" ] && ! echo "$REPLAY" | grep -Eq '"ledger_entries":[1-9]'; then echo "the erasure ledger has entries but the replay read none: do not start the system"; exit 1; fi
$COMPOSE start api worker
echo "restored from $IN; check the health panel and the audit verification before using the system"
