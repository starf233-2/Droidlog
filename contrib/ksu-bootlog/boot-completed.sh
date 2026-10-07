#!/system/bin/sh
# Droidlog boot-log: one-shot snapshots once the boot has finished.
#
# By this point the early userland logs already happened, so a bounded dump of the logcat buffers
# captures what nobody can collect later: the desktop app is not connected yet, and the buffers
# rotate away. That snapshot is the *only* logcat this module keeps -- there is no continuous
# capture, because a stream that grows while the phone is in use belongs on the computer, not in
# the phone's storage.
#
# Everything here is one-shot and bounded, and the only directory written to is /data/adb/droidlog.
#
# Retention is by *set*, not by file count: see keep_newest_sets below. It runs here, in
# post-fs-data.sh and in service.sh, because a boot loop may never reach boot-completed -- and a
# boot loop is exactly the situation this module exists to capture.

MODDIR=${0%/*}
OUT=/data/adb/droidlog
CONF=$OUT/config.env

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
  echo "$STAMP boot-completed: $*" >> "$OUT/logs/module.log" 2>/dev/null
}

mkdir -p "$OUT/boot" "$OUT/logs" "$OUT/kernel" 2>/dev/null || exit 0
usable_dir "$OUT" || exit 0
usable_dir "$OUT/boot" || exit 0
usable_dir "$OUT/kernel" || exit 0
chmod 700 "$OUT" 2>/dev/null

[ -f "$MODDIR/disable" ] && exit 0
[ -f "$OUT/.disabled" ] && exit 0

STAMP=$(date '+%Y%m%d-%H%M%S' 2>/dev/null)
[ -n "$STAMP" ] || STAMP=unknown

# The same fallback post-fs-data.sh has. Without it, a ROM whose toybox lacks `timeout` would
# silently record "snapshot failed" and nothing else, on every boot.
if command -v timeout >/dev/null 2>&1; then
  TMO="timeout"
else
  TMO=""
  log "note: timeout is unavailable, running bounded commands directly"
fi

# ---------------------------------------------------------------------------
# Configuration: read by key, never executed (see service.sh for why)
# ---------------------------------------------------------------------------

# The first `KEY = value` line for KEY, unquoted; non-zero when absent.
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

# Clamps a decimal to [MIN, MAX], falling back to DEF. Long digit strings are rejected by length
# before any arithmetic, because a value that large must not reach logcat.
clamp_int() {
  V=$1
  MIN=$2
  MAX=$3
  DEF=$4
  case "$V" in
    ''|*[!0-9]*) V=$DEF ;;
  esac
  [ ${#V} -gt 6 ] && V=$MAX
  [ "$V" -lt "$MIN" ] 2>/dev/null && V=$MIN
  [ "$V" -gt "$MAX" ] 2>/dev/null && V=$MAX
  printf '%s' "$V"
}

BUFFERS=$(conf_value BUFFERS)
BOOT_LOGCAT=$(conf_value BOOT_LOGCAT)
BOOT_LINES=$(conf_value BOOT_LINES)
FORMAT=$(conf_value FORMAT)

[ -n "$BUFFERS" ] || BUFFERS="crash system events"
[ -n "$FORMAT" ] || FORMAT=threadtime
[ -n "$BOOT_LOGCAT" ] || BOOT_LOGCAT=1
case "$BOOT_LOGCAT" in 0|1) ;; *) BOOT_LOGCAT=1 ;; esac
BOOT_LINES=$(clamp_int "$BOOT_LINES" 200 20000 20000)

# Buffers are whitelisted by name, so the config file cannot introduce arbitrary logcat arguments.
BUFARGS=""
for b in $BUFFERS; do
  case "$b" in
    main|system|crash|events|radio|security|kernel|default) BUFARGS="$BUFARGS -b $b" ;;
    *) log "ignoring unknown buffer '$b'" ;;
  esac
done
[ -n "$BUFARGS" ] || BUFARGS="-b crash"

# Free KB on the filesystem holding $1, empty when unknown.
free_kb() {
  df -Pk "$1" 2>/dev/null | awk 'NR==2 {print $4}' 2>/dev/null
}

# Refuses to write a large dump when /data is nearly full; the dmesg snapshot and the logcat dump
# are convenience, while a full /data is a real problem.
have_space() {
  NEED_KB=$1
  FREE=$(free_kb "$OUT")
  [ -n "$FREE" ] || return 0
  [ "$FREE" -lt "$NEED_KB" ] 2>/dev/null && return 1
  return 0
}

# Keeps the newest KEEP boot sets in DIR and deletes the rest. See service.sh for the reasoning.
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

log "start pid=$$ boot_logcat=$BOOT_LOGCAT lines=$BOOT_LINES buffers='$BUFFERS'"

# Kernel ring buffer as it stands after boot. A snapshot, not a stream. `dmesg -c` is deliberately
# NOT used: clearing the ring would steal the log from every other tool on the device.
if have_space 8192; then
  if $TMO 30 dmesg > "$OUT/boot/$STAMP.dmesg.txt" 2>/dev/null; then
    chmod 600 "$OUT/boot/$STAMP.dmesg.txt" 2>/dev/null
    log "dmesg snapshot bytes=$(wc -c < "$OUT/boot/$STAMP.dmesg.txt" 2>/dev/null)"
    # The fixed-name copy the app's directory probe reads (see post-fs-data.sh for why the kernel
    # evidence lives in its own three-file directory).
    cp "$OUT/boot/$STAMP.dmesg.txt" "$OUT/kernel/dmesg.txt" 2>/dev/null
    chmod 600 "$OUT/kernel/dmesg.txt" 2>/dev/null
  else
    log "dmesg snapshot failed"
  fi
else
  log "skipping dmesg snapshot: less than 8 MB free on /data"
fi

# The one-shot userland snapshot, optional and size-bounded.
if [ "$BOOT_LOGCAT" = "0" ]; then
  log "boot logcat snapshot disabled in config.env (BOOT_LOGCAT=0)"
elif have_space 16384; then
  # shellcheck disable=SC2086
  # The unquoted $BUFARGS is deliberate: it is an argument list, already whitelisted above.
  if $TMO 90 logcat $BUFARGS -d -v "$FORMAT" -t "$BOOT_LINES" \
    > "$OUT/boot/$STAMP.logcat-boot.txt" 2>/dev/null; then
    chmod 600 "$OUT/boot/$STAMP.logcat-boot.txt" 2>/dev/null
    log "logcat boot snapshot bytes=$(wc -c < "$OUT/boot/$STAMP.logcat-boot.txt" 2>/dev/null)"
  else
    log "logcat boot snapshot failed"
  fi
else
  log "skipping logcat boot snapshot: less than 16 MB free on /data"
fi

# Properties that make a crash report self-explanatory later.
{
  echo "captured_at=$(date 2>/dev/null)"
  echo "boot_completed=$(timeout 5 getprop sys.boot_completed 2>/dev/null)"
  echo "bootreason=$(timeout 5 getprop ro.boot.bootreason 2>/dev/null)"
  echo "build=$(timeout 5 getprop ro.build.display.id 2>/dev/null)"
  echo "fingerprint=$(timeout 5 getprop ro.build.fingerprint 2>/dev/null)"
  echo "kernel=$(timeout 5 cat /proc/version 2>/dev/null)"
  echo "uptime=$(timeout 5 cat /proc/uptime 2>/dev/null)"
  echo "stamp=$STAMP"
} > "$OUT/boot/$STAMP.props.txt" 2>/dev/null
chmod 600 "$OUT/boot/$STAMP.props.txt" 2>/dev/null

echo "$STAMP" > "$OUT/boot/latest" 2>/dev/null

# Retention: newest sets win; only names this module produces are considered.
keep_newest_sets "$OUT/boot" 3 80
# kernel/ holds three fixed names, one per kind, overwritten every boot: nothing to rotate there.

log "done"
exit 0
