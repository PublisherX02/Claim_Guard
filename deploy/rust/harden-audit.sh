#!/usr/bin/env bash
# Makes the audit logs append-only at the operating-system level (docs/58). Run as root on the Linux host.
#
#   harden-audit.sh apply  DIR [DIR...]   set the append-only attribute on every *.jsonl in the directories
#   harden-audit.sh status DIR [DIR...]   show which files are append-only and which are not (exit 1 if any is not)
#   harden-audit.sh lift   DIR [DIR...]   remove the attribute (for log rotation or restore; needs root, and is itself something to log)
#
# With the attribute set the kernel refuses to truncate, overwrite in place, rename, link or delete the file, even for root, until root lifts it.
# Appending still works, which is all the writer ever does. Cover the live logs (the security log, the review log, the ledger checkpoints)
# and the mirror directory. The anchor files are rewritten by rename on every append and are signed with the anchor key, so they stay writable.
# Needs a filesystem with attribute support (ext4, xfs, btrfs). Run `apply` again after a restore or after new logs appear.
set -euo pipefail
cmd="${1:-}"; shift || true
[ -n "$cmd" ] && [ $# -ge 1 ] || { sed -n '2,10p' "$0"; exit 2; }
command -v chattr >/dev/null || { echo "chattr not found (install e2fsprogs)"; exit 2; }
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 2; }
bad=0
for d in "$@"; do
  [ -d "$d" ] || { echo "not a directory: $d"; exit 2; }
  while IFS= read -r -d '' f; do
    case "$cmd" in
      apply)  chattr +a "$f" && echo "append-only: $f" ;;
      lift)   chattr -a "$f" && echo "lifted: $f" ;;
      status) if lsattr -d "$f" | cut -d' ' -f1 | grep -q a; then echo "append-only: $f"; else echo "WRITABLE:    $f"; bad=1; fi ;;
      *) echo "unknown command $cmd"; exit 2 ;;
    esac
  done < <(find "$d" -maxdepth 2 -type f -name '*.jsonl' -print0)
done
exit $bad
