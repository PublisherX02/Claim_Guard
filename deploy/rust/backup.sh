#!/usr/bin/env bash
# Encrypted backup of the Rust stack: MongoDB (users, claims, queue state), the audit logs and their mirror, in ONE file.
#   BACKUP_KEY_FILE=/root/claimguard-backup.key bash deploy/rust/backup.sh /srv/backups/claimguard-$(date -u +%Y%m%d).tar.enc
# The key file holds a long random passphrase (openssl rand -base64 48 > file; chmod 600). Without it the backup cannot be read, so store it
# somewhere other than the backup. The backup does NOT contain the secrets in .env (JWT, Fernet, PII, audit-anchor and data keys): back
# those up separately, because without DATA_KEY/FERNET_KEY the encrypted claim data and the authenticator seeds cannot be read.
# The file is encrypted (AES-256-CBC, PBKDF2 600,000 rounds) and a SHA-256 of the encrypted file is written beside it.
#
# THE PATIENT KEYS ARE BACKED UP IN A FILE OF THEIR OWN (<output>.keys), never inside the main file (docs/59). Each patient's claim data is
# encrypted under that patient's own key, so erasing a patient means destroying their key. That only protects the backups if the keys are
# not in them: keep the .keys files for a SHORT time (30 days is the suggestion) and the data backups for as long as you need. Once the last
# .keys file that held a patient's key is deleted, that patient's data is unreadable in every older data backup. A data backup without a
# matching .keys file cannot be read, so keep each pair for at least the short period.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT="${1:?usage: BACKUP_KEY_FILE=<file> backup.sh <output file>}"
KEY="${BACKUP_KEY_FILE:?set BACKUP_KEY_FILE to a file holding the backup passphrase}"
[ -s "$KEY" ] || { echo "the key file is missing or empty"; exit 2; }
[ ! -e "$OUT" ] || { echo "$OUT already exists: not overwriting"; exit 2; }
PROJECT="${COMPOSE_PROJECT:-claimguard}"
ENVFILE="${ENV_FILE:-.env}"
COMPOSE="docker compose -f deploy/rust/compose.yml --env-file $ENVFILE -p $PROJECT"
umask 077
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
HOSTTMP="$TMP"; command -v cygpath >/dev/null 2>&1 && HOSTTMP="$(cygpath -w "$TMP")"
# only the two application databases, one archive each: a whole-server dump would also carry (and a restore would replace) the database's own
# user accounts, which pulls the rug from under the restore itself
DB="$(grep -E '^MONGO_DB=' "$ENVFILE" | cut -d= -f2- || true)"; DB="${DB:-claimguard}"
QDB="$(grep -E '^QUEUE_DB=' "$ENVFILE" | cut -d= -f2- || true)"; QDB="${QDB:-${DB}_queue}"
for name in "$DB" "$QDB"; do
  $COMPOSE exec -T mongo sh -c "mongodump --archive --gzip --db '$name' -u \"\$MONGO_INITDB_ROOT_USERNAME\" -p \"\$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin" > "$TMP/mongo-$name.archive.gz" 2>/dev/null
  [ -s "$TMP/mongo-$name.archive.gz" ] || { echo "the dump of $name is empty"; exit 1; }
done
# the patient keys: their own database, their own file, their own (short) life
KDB="$(grep -E '^KEYS_DB=' "$ENVFILE" | cut -d= -f2- || true)"; KDB="${KDB:-${QDB}_keys}"
$COMPOSE exec -T mongo sh -c "mongodump --archive --gzip --db '$KDB' -u \"\$MONGO_INITDB_ROOT_USERNAME\" -p \"\$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin" > "$TMP/keys.archive.gz" 2>/dev/null
[ -s "$TMP/keys.archive.gz" ] || { echo "the dump of $KDB is empty"; exit 1; }
MSYS_NO_PATHCONV=1 docker run --rm -v "${PROJECT}_claimguard_data:/data:ro" -v "${PROJECT}_audit_mirror:/mirror:ro" -v "$HOSTTMP":/out alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 sh -c 'tar czf /out/logs.tgz -C / data mirror'
( cd "$TMP" && sha256sum mongo-*.archive.gz logs.tgz > SHA256SUMS && tar cf payload.tar mongo-*.archive.gz logs.tgz SHA256SUMS )
openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt -pass "file:$KEY" -in "$TMP/payload.tar" -out "$OUT"
( cd "$(dirname "$OUT")" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256" )
[ ! -e "$OUT.keys" ] || { echo "$OUT.keys already exists: not overwriting"; exit 2; }
openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt -pass "file:$KEY" -in "$TMP/keys.archive.gz" -out "$OUT.keys"
( cd "$(dirname "$OUT")" && sha256sum "$(basename "$OUT").keys" > "$(basename "$OUT").keys.sha256" )
echo "patient keys written to $OUT.keys: delete this file after the short retention period, not when the data backup is deleted"
echo "encrypted backup written to $OUT ($(wc -c < "$OUT") bytes); its checksum is in $OUT.sha256"
