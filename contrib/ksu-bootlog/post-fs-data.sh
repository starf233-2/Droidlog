#!/system/bin/sh
# Droidlog boot-log: rescue the kernel's crash evidence as early as possible.
#
# This runs at post-fs-data, which is ON THE BOOT PATH: the boot waits for it. So it is short,
# never waits on anything, never loops, and every external command is bounded.
#
# It writes nothing outside /data/adb/droidlog, and it does not execute its configuration file
# (see service.sh for the reasoning: the file used to be sourced, which made it a root-owned
# persistence point and let a stray quote kill the capture silently).
#
# The config parser is duplicated here on purpose rather than sourced from a shared file: a boot
# script that must not fail should not depend on another file being present and parseable.

MODDIR=${0%/*}
OUT=/data/adb/droidlog
CONF=$OUT/config.env
PSTORE=/sys/fs/pstore

umask 077

# True only for a real directory that is not a symlink; see service.sh.
usable_dir() {
  [ -n "$1" ] || return 1
  [ -L "$1" ] && return 1
  [ -d "$1" ] || return 1
  return 0
}

log() {
  [ -n "$STAMP" ] || STAMP=unknown
  echo "$STAMP post-fs-data: $*" >> "$OUT/logs/module.log" 2>/dev/null
}

mkdir -p "$OUT/boot" "$OUT/logs" "$OUT/kernel" 2>/dev/null || exit 0
usable_dir "$OUT" || exit 0
usable_dir "$OUT/boot" || exit 0
usable_dir "$OUT/kernel" || exit 0
chmod 700 "$OUT" 2>/dev/null

[ -f "$MODDIR/disable" ] && exit 0
[ -f "$OUT/.disabled" ] && exit 0

# The clock is not necessarily set this early: on the test device `date` returned 1970-02-14 at
# post-fs-data, which would have stamped the *most valuable* snapshot (the rescued pstore) with the
# oldest name in the directory, so retention would delete it first. When the year looks unset the
# stamp records uptime instead: monotonic, still sortable, and clearly not a wall-clock time.
# boot-completed.sh runs with a set clock and writes the real time into the metadata.
STAMP=$(date '+%Y%m%d-%H%M%S' 2>/dev/null)
YEAR=${STAMP%%-*}
case "$YEAR" in
  19*|200*|201[0-9])
    UP=$(cut -d. -f1 /proc/uptime 2>/dev/null)
    [ -n "$UP" ] || UP=0
    STAMP=$(printf 'bootup-%08d' "$UP" 2>/dev/null)
    ;;
esac
[ -n "$STAMP" ] || STAMP=unknown

# `timeout` is toybox's; fall back to a bare command when it is missing (pstore regions are a
# fixed few hundred KB, so the fallback cannot run away).
if command -v timeout >/dev/null 2>&1; then
  TMO="timeout 10"
else
  TMO=""
  log "note: timeout is unavailable, running bounded commands directly"
fi

# The first `KEY = value` line for KEY, unquoted; non-zero when absent. Same parser as service.sh.
conf_value() {
  KEY=$1
  [ -f "$CONF" ] || return 1
  LINE=$(grep -m 1 "^[[:space:]]*$KEY[[:space:]]*=" "$CONF" 2>/dev/null) || return 1
  [ -n "$LINE" ] || return 1
  VALUE=${LINE#*=}
  VALUE=$(printf '%s' "$VALUE" | tr -d '\r')
  case "$VALUE" in
    \"*\") VALUE=${VALUE#\"}; VALUE=${VALUE%\"} ;;
    \'*\') VALUE=${VALUE#\'}; VALUE=${VALUE%\'} ;;
  esac
  printf '%s' "$VALUE"
}

# pmsg-ramoops holds the previous boot's *userland* logcat, which may include buffers the user did
# not select, so it can be switched off from the config.
PMSG=$(conf_value PMSG)
[ -n "$PMSG" ] || PMSG=1
case "$PMSG" in 0|1) ;; *) PMSG=1 ;; esac

# Free KB on the filesystem holding $1, empty when unknown.
free_kb() {
  df -Pk "$1" 2>/dev/null | awk 'NR==2 {print $4}' 2>/dev/null
}

# Refuses to write when the filesystem is nearly full: filling /data would make the very failure
# this module exists to diagnose much harder to recover from.
have_space() {
  NEED_KB=$1
  FREE=$(free_kb "$OUT")
  [ -n "$FREE" ] || return 0
  [ "$FREE" -lt "$NEED_KB" ] 2>/dev/null && return 1
  return 0
}

# Bounded cleanup, here as well as in service.sh: a boot loop may never reach the late_start
# service, and this script writes a snapshot every round. At most MAX_SCAN entries are examined, so
# the boot path stays short.
# Keeps the newest KEEP boot sets in DIR and deletes the rest.
#
# A "set" is every file sharing one stamp (the part before the first dot), which is what a single
# boot produces: meta, dmesg, logcat dump, props, and any rescued pstore file. The files of one set
# are consecutive once names are sorted, so a single pass can rank them without a second lookup.
#
# The previous version kept a flat count of *files*, which could keep the tail of an old boot while
# dropping the head of the newest one -- and a partial record is worse than no record, because it
# still looks complete. MAX_SCAN bounds the work: this runs on the boot path in post-fs-data.sh.
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

keep_newest_sets "$OUT/boot" 3 80
# kernel/ holds three fixed names, one per kind, overwritten every boot: nothing to rotate there.

# Trims the module log without letting it grow without bound.
if [ -f "$OUT/logs/module.log" ]; then
  LINES=$(wc -l < "$OUT/logs/module.log" 2>/dev/null)
  if [ -n "$LINES" ] && [ "$LINES" -gt 400 ] 2>/dev/null; then
    tail -n 400 "$OUT/logs/module.log" > "$OUT/logs/module.log.trim" 2>/dev/null &&
      mv "$OUT/logs/module.log.trim" "$OUT/logs/module.log" 2>/dev/null
  fi
fi

log "start pid=$$ pmsg=$PMSG"

# Metadata first, so even a failed copy leaves a record of *why* this boot happened.
{
  echo "captured_at=$(date 2>/dev/null)"
  echo "bootreason=$(timeout 5 getprop ro.boot.bootreason 2>/dev/null)"
  echo "boot_completed=$(timeout 5 getprop sys.boot_completed 2>/dev/null)"
  echo "kernel=$(timeout 5 cat /proc/version 2>/dev/null)"
  echo "stamp=$STAMP"
} > "$OUT/boot/$STAMP.meta" 2>/dev/null

# 1) The previous boot's kernel log, if the kernel kept one. This is the whole point: it is
#    non-empty only after an abnormal reboot (panic, watchdog, hard reset), and the system may
#    clear it later in the boot, which is why it is copied here and not by the app.
if [ -d "$PSTORE" ]; then
  FOUND=0
  SKIPPED=0
  for f in "$PSTORE"/*; do
    [ -f "$f" ] || continue
    NAME=$(basename "$f" 2>/dev/null)
    [ -n "$NAME" ] || continue
    case "$NAME" in
      pmsg-ramoops*)
        if [ "$PMSG" = "0" ]; then
          SKIPPED=$((SKIPPED + 1))
          continue
        fi
        ;;
    esac
    if ! have_space 4096; then
      log "skipping pstore/$NAME: less than 4 MB free on /data"
      continue
    fi
    if $TMO cat "$f" > "$OUT/boot/$STAMP.pstore-$NAME.txt" 2>/dev/null; then
      chmod 600 "$OUT/boot/$STAMP.pstore-$NAME.txt" 2>/dev/null
      FOUND=1
      SIZE=$(wc -c < "$OUT/boot/$STAMP.pstore-$NAME.txt" 2>/dev/null)
      log "rescued pstore/$NAME bytes=${SIZE:-?}"
    else
      log "could not read pstore/$NAME"
    fi
  done
  [ "$FOUND" = "1" ] || log "pstore present but nothing rescued (clean shutdown, or all skipped)"
  [ "$SKIPPED" -gt 0 ] && log "skipped $SKIPPED pmsg file(s): PMSG=0 in config.env"
else
  log "no $PSTORE on this kernel"
fi

# 2) The older single-file interface, still present on some kernels.
if [ -f /proc/last_kmsg ]; then
  if have_space 2048 && $TMO cat /proc/last_kmsg > "$OUT/boot/$STAMP.last_kmsg.txt" 2>/dev/null; then
    chmod 600 "$OUT/boot/$STAMP.last_kmsg.txt" 2>/dev/null
    log "rescued /proc/last_kmsg"
  else
    log "could not read /proc/last_kmsg (or not enough space)"
  fi
fi

# 3) The two fixed-name copies the app reads.
#
#    A reader such as Droidlog lists a directory and takes the first few files, so a directory of
#    timestamped names would hand it the oldest boot's files, and mixing megabyte userland dumps in
#    with the small kernel files would spend its whole budget on those. This directory holds exactly
#    three files, one per kind, overwritten each boot, which is also why nothing has to be deleted
#    to keep it correct.
#
#    Both are truncated first: a stale pstore from an earlier boot shown as this boot's would be
#    worse than an empty file. Empty means "the previous shutdown was clean", and the stamped reason
#    is written to boot/<stamp>.meta either way.
: > "$OUT/kernel/pstore.txt" 2>/dev/null
: > "$OUT/kernel/last_kmsg.txt" 2>/dev/null
chmod 600 "$OUT/kernel/pstore.txt" "$OUT/kernel/last_kmsg.txt" 2>/dev/null

if [ -d "$PSTORE" ] && have_space 2048; then
  for f in "$PSTORE"/*; do
    [ -f "$f" ] || continue
    NAME=$(basename "$f" 2>/dev/null)
    case "$NAME" in
      pmsg-ramoops*)
        [ "$PMSG" = "0" ] && continue
        ;;
    esac
    $TMO cat "$f" >> "$OUT/kernel/pstore.txt" 2>/dev/null
  done
fi
if [ -f /proc/last_kmsg ] && have_space 1024; then
  $TMO cat /proc/last_kmsg >> "$OUT/kernel/last_kmsg.txt" 2>/dev/null
fi

# A pointer the app can read to find the newest snapshot without listing the directory.
echo "$STAMP" > "$OUT/boot/latest" 2>/dev/null

log "done"
exit 0
