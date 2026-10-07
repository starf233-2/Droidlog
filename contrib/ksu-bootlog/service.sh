#!/system/bin/sh
# Droidlog boot-log: post-boot housekeeping.
#
# This script used to start a continuous logcat writer. It deliberately no longer does: the phone's
# own storage is the wrong place for a stream the desktop app can capture directly, and that file
# grew for as long as the phone was in use. What this module keeps is only the evidence the app
# *cannot* obtain later:
#
#   * the previous boot's kernel log, rescued at post-fs-data before the system clears it;
#   * this boot's kernel ring, and a one-shot snapshot of the early userland logs, both taken at
#     boot-completed -- a window that is already over by the time anyone connects.
#
# So this is now the housekeeping that must happen on every boot (retention and log trimming), plus
# one migration step: it stops a writer left behind by an earlier version, which is what makes the
# change take effect on an existing install without editing it by hand.
#
# Written paths: /data/adb/droidlog only.

MODDIR=${0%/*}
OUT=/data/adb/droidlog
CONF=$OUT/config.env

# The data directory is created 0700 and the files 0600: `/data/adb` is 0700 root, but the module
# does not lean on that.
umask 077

# True only for a real directory that is not a symlink. A symlinked data directory would redirect
# every write and every delete in this module, so nothing destructive happens until this passes.
usable_dir() {
  [ -n "$1" ] || return 1
  [ -L "$1" ] && return 1
  [ -d "$1" ] || return 1
  return 0
}

log() {
  [ -n "$STAMP" ] || STAMP=unknown
  echo "$STAMP service: $*" >> "$OUT/logs/module.log" 2>/dev/null
}

mkdir -p "$OUT/logs" "$OUT/run" 2>/dev/null || exit 0
usable_dir "$OUT" || exit 0
chmod 700 "$OUT" 2>/dev/null

[ -f "$MODDIR/disable" ] && exit 0
[ -f "$OUT/.disabled" ] && exit 0

STAMP=$(date '+%Y%m%d-%H%M%S' 2>/dev/null)
[ -n "$STAMP" ] || STAMP=unknown

# ---------------------------------------------------------------------------
# Configuration: read by key, never executed
#
# The file is PARSED, not sourced: the first version used `. "$CONF"`, which made it shell code
# running as root at every boot, and let a stray quote kill the service silently.
# ---------------------------------------------------------------------------

if [ ! -f "$CONF" ]; then
  cat > "$CONF" <<'EOF'
# Droidlog boot-log configuration.
#
# This file is READ, not executed: only the keys below matter, and a line that is not
# `KEY = value` is ignored. Values are clamped, so a typo cannot fill the disk.
#
# The module does NOT capture logcat continuously -- the desktop app does that directly. It only
# takes a one-shot snapshot at boot, which is the part the app cannot get afterwards.

# Buffers for the boot snapshot, space separated.
# Allowed: main system crash events radio security kernel
# Default is crash+system+events. Adding `main` or `radio` makes the snapshot much larger.
BUFFERS=crash system events

# Take the boot snapshot at all. Set to 0 for zero logcat storage: the kernel evidence
# (pstore/dmesg) is still collected either way.
BOOT_LOGCAT=1

# Lines kept from the boot snapshot (200-20000). This is the only logcat size knob.
BOOT_LINES=20000

# logcat -v format for the snapshot.
FORMAT=threadtime

# Copy pstore's pmsg-ramoops files, which hold the *previous* boot's userland logcat (possibly
# including the main buffer, regardless of BUFFERS). Set to 0 to skip them.
PMSG=1
EOF
  chmod 600 "$CONF" 2>/dev/null
fi

# Unconditional, not only on creation: a config left readable by an earlier version of this module
# (or written by hand) would stay that way otherwise, and it is the one file here a reader edits.
chmod 600 "$CONF" 2>/dev/null

# ---------------------------------------------------------------------------
# Retention and log trimming
# ---------------------------------------------------------------------------

# Keeps the newest KEEP boot sets in DIR and deletes the rest.
#
# A "set" is every file sharing one stamp (the part before the first dot), which is what a single
# boot produces: meta, dmesg, logcat snapshot, props, and any rescued pstore file. The files of one
# set are consecutive once names are sorted, so a single pass can rank them without a second lookup.
#
# Keeping a flat count of *files* instead would keep the tail of an old boot while dropping the head
# of the newest one -- and a partial record is worse than no record, because it still looks
# complete. MAX_SCAN bounds the work: post-fs-data.sh runs this on the boot path.
#
# Only names this module produces are considered, so a file the user put in the directory is left
# alone. Symbolic links are never followed.
keep_newest_sets() {
  DIR=$1
  KEEP=$2
  MAX_SCAN=$3
  usable_dir "$DIR" || return 0
  SCANNED=0
  RANK=0
  CURRENT=""
  GONE=0
  for NAME in $(ls -1r "$DIR" 2>/dev/null); do
    SCANNED=$((SCANNED + 1))
    [ "$SCANNED" -gt "$MAX_SCAN" ] && break
    case "$NAME" in
      [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9].*|bootup-*) ;;
      *) continue ;;
    esac
    STAMP=${NAME%%.*}
    if [ "$STAMP" != "$CURRENT" ]; then
      CURRENT=$STAMP
      RANK=$((RANK + 1))
    fi
    [ "$RANK" -le "$KEEP" ] && continue
    FILE=$DIR/$NAME
    [ -f "$FILE" ] || continue
    [ -L "$FILE" ] && continue
    rm -f "$FILE" 2>/dev/null && GONE=$((GONE + 1))
  done
  [ "$GONE" -gt 0 ] && log "cleanup: removed $GONE file(s) from boots older than the newest $KEEP in $DIR"
  return 0
}

# Trims a log file to its last MAX_LINES lines.
trim_log() {
  FILE=$1
  MAX_LINES=$2
  [ -f "$FILE" ] || return 0
  [ -L "$FILE" ] && return 0
  LINES=$(wc -l < "$FILE" 2>/dev/null)
  [ -n "$LINES" ] || return 0
  if [ "$LINES" -gt "$MAX_LINES" ]; then
    # The temporary file has a fixed name inside $OUT rather than "$FILE.trim": a write target the
    # build-time check can recognise is a write target that cannot quietly become something else.
    tail -n "$MAX_LINES" "$FILE" > "$OUT/logs/.trim.$$" 2>/dev/null &&
      mv "$OUT/logs/.trim.$$" "$FILE" 2>/dev/null
  fi
  return 0
}

keep_newest_sets "$OUT/boot" 3 80
# kernel/ holds three fixed names, one per kind, overwritten every boot: nothing to rotate there.
trim_log "$OUT/logs/module.log" 400
trim_log "$OUT/logs/logcat.out" 200

# ---------------------------------------------------------------------------
# Migration: stop a writer started by an earlier version
#
# Older versions ran `logcat -f live/logcat.txt`, which is exactly the file that grew while the phone
# was in use. Those installs leave a pid file behind, so this stops that process once and never
# starts another. The files it wrote are left in place: removing data is the action button's job (or
# `ctl.sh purge`), not an upgrade's -- an upgrade should not silently delete what it finds.
# ---------------------------------------------------------------------------

# True when $1 exists and its command line names this module's live directory.
is_ours() {
  PID=$1
  [ -n "$PID" ] || return 1
  case "$PID" in *[!0-9]*) return 1 ;; esac
  [ -d "/proc/$PID" ] || return 1
  CMD=$(tr '\0' ' ' < "/proc/$PID/cmdline" 2>/dev/null)
  case "$CMD" in *"$OUT/live/"*) return 0 ;; *) return 1 ;; esac
}

PIDF=$OUT/run/logcat.pid
if [ -f "$PIDF" ]; then
  PID=$(cat "$PIDF" 2>/dev/null)
  if is_ours "$PID"; then
    kill "$PID" 2>/dev/null
    sleep 1
    if is_ours "$PID"; then
      kill -9 "$PID" 2>/dev/null
    fi
    log "stopped the logcat writer left by an earlier version (pid=$PID); no live capture any more"
  fi
  rm -f "$PIDF" 2>/dev/null
fi

log "housekeeping done (this version stores boot evidence only)"

exit 0
