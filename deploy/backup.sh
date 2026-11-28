#!/bin/sh
# Back up Cairn's namespace from EoC's index, and upload it to S3 or R2 (docs/deploy.md,
# "Backups"). Weekly from the host's crontab, e.g. Sundays at 03:17:
#   17 3 * * 0  cd ~/cairn && sh deploy/backup.sh >> backups/backup.log 2>&1
#
#   sh deploy/backup.sh               export, check, compress, upload, prune local copies
#   sh deploy/backup.sh --no-upload   the same without the upload (`make vps-backup`)
#
# Settings: deploy/cairn.env (SURREAL_USER / SURREAL_PASS: Cairn's own database user, so
# no root password is used) and deploy/backup.env (where to, and rclone's S3 settings).
# EOC=<EoC's folder> if it isn't ../engine-of-creation. KEEP_DAYS for local copies.
set -eu
cd "$(dirname "$0")/.."

EOC="${EOC:-../engine-of-creation}"
KEEP_DAYS="${KEEP_DAYS:-35}"
RCLONE_IMAGE="${RCLONE_IMAGE:-rclone/rclone:1.68.2}"
upload=yes
[ "${1:-}" = "--no-upload" ] && upload=no

# One KEY=value line from an env file, taken literally. The files are not sourced:
# CAIRN_TOKENS holds `;`, which a shell would run as a command separator.
conf() { sed -n "s/^$2=//p" "$1" | tail -n 1; }

user="$(conf deploy/cairn.env SURREAL_USER)"
pass="$(conf deploy/cairn.env SURREAL_PASS)"
[ -n "$user" ] && [ -n "$pass" ] || { echo "backup: SURREAL_USER/SURREAL_PASS missing in deploy/cairn.env" >&2; exit 1; }
if [ "$upload" = yes ]; then
  dest="$(conf deploy/backup.env CAIRN_BACKUP_DEST)"
  [ -n "$dest" ] || { echo "backup: CAIRN_BACKUP_DEST missing in deploy/backup.env" >&2; exit 1; }
fi

mkdir -p backups
name="cairn-$(date -u +%Y-%m-%dT%H%MZ).surql"
part="backups/$name.partial"
trap 'rm -f "$part"' EXIT          # a failed export leaves nothing behind

# `surreal export` inside EoC's index container, as Cairn's database user. `--users false`:
# exports redact password hashes (`PASSHASH '[REDACTED]'`), so a restored user would have a
# broken password. The user comes from surreal-init.surql instead.
if ! docker compose --project-directory "$EOC" exec -T index /surreal export \
    --endpoint http://localhost:8000 --user "$user" --pass "$pass" --auth-level database \
    --namespace cairn --database main --users false - > "$part"; then
  echo "backup: export failed; nothing uploaded" >&2
  exit 1
fi
# An export always defines Cairn's tables; anything without them is not a backup.
if ! grep -q "DEFINE TABLE item" "$part"; then
  echo "backup: export looks empty or truncated ($(wc -c < "$part") bytes); nothing uploaded" >&2
  exit 1
fi
mv "$part" "backups/$name"
gzip -9 "backups/$name"
echo "backup: wrote backups/$name.gz ($(wc -c < "backups/$name.gz") bytes)"

if [ "$upload" = yes ]; then
  # rclone reads its S3 settings (RCLONE_CONFIG_*) from deploy/backup.env. The two flags
  # let a write-only key work: no look at the destination before the upload, and no HEAD
  # after it. S3/R2 still verify each upload against its MD5, and rclone fails if they
  # reject it.
  docker run --rm --env-file deploy/backup.env -v "$PWD/backups:/backups:ro" "$RCLONE_IMAGE" \
    copyto --no-check-dest --s3-no-head "/backups/$name.gz" "$dest/$name.gz"
  echo "backup: uploaded to $dest/$name.gz"
fi

# Local copies only; the bucket's own lifecycle rule ages out remote ones.
find backups -name 'cairn-*.surql.gz' -mtime +"$KEEP_DAYS" -print -delete
