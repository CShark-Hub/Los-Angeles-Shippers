#!/usr/bin/env bash
# The nightly backup of the core's database, as docs/deploy.md describes it: `admin backup` while
# the core serves, the copy taken out of the volume (the volume keeps none) under a .partial name it
# trades for its own only once it is whole, so a copy cut off midway is never counted as one, and the
# newest copies kept in a folder off the volume, older ones deleted. Run it from cron on a server, by
# hand on a laptop; shipping the folder off the machine is the operator's (restic, rclone, a
# USB stick).
# A failed step stops it with a non-zero exit, so no stale copy is kept as the newest; the copy made
# on the core's volume is removed whether or not the copy out of it worked, so a failed night leaves
# nothing there. It lists the copies with the shell alone, so it runs the same with BSD tools.
#
#   backup.sh [--dry-run]    --dry-run says what it would run and delete, and changes nothing
#
#   BACKUP_DIR    where the copies are kept (default: deploy/compose/backups)
#   BACKUP_KEEP   how many copies to keep (default: 14, a fortnight of nights)
#   COMPOSE_FILE  the stack's Compose files, as for docker compose (default: .env's)
set -euo pipefail
# Git Bash on Windows would turn the container paths below (/data) into its own
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")/.."

dry=false
case "${1:-}" in
  "") ;;
  --dry-run) dry=true ;;
  *)
    echo "usage: backup.sh [--dry-run]" >&2
    exit 2
    ;;
esac
dir=${BACKUP_DIR:-backups}
keep=${BACKUP_KEEP:-14}
[[ $keep =~ ^[1-9][0-9]*$ ]] || {
  echo "backup: BACKUP_KEEP is a number of copies, at least 1, not $keep" >&2
  exit 2
}

name=mercury-$(date +%F-%H%M%S).db
run() {
  if $dry; then echo "would run: $*"; else "$@"; fi
}

# As the script exits, after a failed step too (which keeps that step's non-zero status): this run's
# copy cut off on its way out of the volume is deleted, and once `admin backup` has made the copy on
# the core's volume, that goes whatever happened after.
made=false
clean_up() {
  if ! $dry && [ -e "$dir/$name.partial" ]; then
    rm -f "$dir/$name.partial" || echo "backup: the incomplete copy $dir/$name.partial is left: remove it by hand" >&2
  fi
  if $made; then
    docker compose exec -T core rm -f "/data/backups/$name" >/dev/null 2>&1 ||
      echo "backup: /data/backups/$name is still on the core's volume: docker compose exec core rm /data/backups/$name removes it" >&2
  fi
}
trap clean_up EXIT

run mkdir -p "$dir"
run docker compose exec -T core mercury-core admin backup --to "/data/backups/$name" # VACUUM INTO, the core keeps serving
$dry || made=true
run docker compose cp "core:/data/backups/$name" "$dir/$name.partial"
run mv "$dir/$name.partial" "$dir/$name" # whole now: only this name is counted as a copy
run docker compose exec -T core rm "/data/backups/$name" # the volume keeps no copy
made=false

# the newest $keep copies stay; the names sort by their time. A glob, not find -printf, which BSD's
# find lacks.
mapfile -t copies < <(
  {
    if [ -d "$dir" ]; then
      for copy in "$dir"/mercury-*.db; do
        if [ -f "$copy" ]; then echo "${copy##*/}"; fi
      done
    fi
    if $dry; then echo "$name"; fi
  } | sort -r
)
old=("${copies[@]:$keep}")
for copy in "${old[@]}"; do
  run rm -f "$dir/$copy"
done
if $dry; then
  echo "dry run: ${#copies[@]} copies with this one, ${#old[@]} older than the newest $keep would be deleted"
else
  echo "backup: $dir/$name, $((${#copies[@]} - ${#old[@]})) copies kept"
fi
