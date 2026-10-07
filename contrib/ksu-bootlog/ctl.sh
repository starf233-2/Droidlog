#!/system/bin/sh
# Droidlog Boot Log: manual control.
#
# Run by hand from a root shell (or the KernelSU app's action). Nothing here runs automatically at
# boot, so it carries no boot risk at all.
#
#   sh ctl.sh status     what is running and how much has been captured
#   sh ctl.sh start      start the capture now (same as service.sh)
#   sh ctl.sh stop       stop the capture
#   sh ctl.sh restart    stop, then start
#   sh ctl.sh snapshot   take the one-shot snapshots now (same as boot-completed.sh)
#   sh ctl.sh confcheck  show the effective capture settings and flag anything odd in config.env
#   sh ctl.sh sizes      how much space the collected data uses, per directory
#   sh ctl.sh purge      delete all collected data (keeps config.env, keeps the module)
#   sh ctl.sh disable    stop and stay off across reboots
#   sh ctl.sh enable     undo disable
#
# Every path touched is inside /data/adb/droidlog; nothing is written anywhere else.

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

say() { echo "$1"; }

# The first `KEY = value` line for KEY, unquoted. Same parser as service.sh, duplicated here so
# `confcheck` reports what the capture will actually use rather than what a second implementation
# thinks it would use.
conf_lookup() {
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

# True when $1 exists and its command line names this module's live directory.
is_ours() {
  PID=$1
  [ -n "$PID" ] || return 1
  case "$PID" in *[!0-9]*) return 1 ;; esac
  [ -d "/proc/$PID" ] || return 1
  CMD=$(tr '\0' ' ' < "/proc/$PID/cmdline" 2>/dev/null)
  case "$CMD" in *"$OUT/live/"*) return 0 ;; *) return 1 ;; esac
}

cmd_stop() {
  STOPPED=0
  for p in "$OUT"/run/*.pid; do
    [ -f "$p" ] || continue
    PID=$(cat "$p" 2>/dev/null)
    if is_ours "$PID"; then
      kill "$PID" 2>/dev/null
      sleep 1
      # Re-checked, as in service.sh: a recycled pid must not be killed by us.
      if is_ours "$PID"; then
        kill -9 "$PID" 2>/dev/null
      fi
      STOPPED=$((STOPPED + 1))
      say "stopped pid $PID"
    fi
    rm -f "$p" 2>/dev/null
  done
  [ "$STOPPED" = "0" ] && say "nothing was running"
  return 0
}

cmd_start() {
  if [ -f "$MODDIR/disable" ] || [ -f "$OUT/.disabled" ]; then
    say "module is disabled; run: sh ctl.sh enable"
    return 0
  fi
  sh "$MODDIR/service.sh"
  say "housekeeping run (no background capture to start)"
}

cmd_status() {
  say "module dir : $MODDIR"
  [ -f "$MODDIR/disable" ] && say "ksu flag   : DISABLED (module dir has 'disable')"
  [ -f "$OUT/.disabled" ] && say "our flag   : DISABLED ($OUT/.disabled)"
  say "data dir   : $OUT"
  RUNNING=0
  for p in "$OUT"/run/*.pid; do
    [ -f "$p" ] || continue
    PID=$(cat "$p" 2>/dev/null)
    if is_ours "$PID"; then
      RUNNING=$((RUNNING + 1))
      say "running    : $(basename "$p") pid=$PID"
    fi
  done
  [ "$RUNNING" = "0" ] && say "capture    : boot-time only (this version runs no background process)"
  say "pstore     : $([ -d /sys/fs/pstore ] && ls /sys/fs/pstore 2>/dev/null | wc -l || echo absent) file(s)"
  say "latest boot: $(cat "$OUT/boot/latest" 2>/dev/null)"
  say "--- last 10 module log lines ---"
  tail -n 10 "$OUT/logs/module.log" 2>/dev/null
}

# Clamps a decimal to [MIN, MAX], falling back to DEF. Same rules as service.sh.
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

# Reports the effective settings and whether config.env contains anything a plain parser ignores.
#
# This exists because the configuration file is no longer executed: a line that is not
# `KEY = value` now does nothing at all, and the only way to notice a stray line is to be told.
# Raw and effective values are both printed, because a value out of range is silently clamped at
# capture time and a reader deserves to see that happen.
cmd_confcheck() {
  say "config file: $CONF"
  if [ ! -f "$CONF" ]; then
    say "  not present yet; service.sh creates it on first start (mode 600)"
    return 0
  fi
  say "  mode: $(ls -l "$CONF" 2>/dev/null | awk '{print $1}')"
  RAW_BUF=$(conf_lookup BUFFERS)
  RAW_DUMP=$(conf_lookup BOOT_LOGCAT)
  RAW_LINES=$(conf_lookup BOOT_LINES)
  RAW_PMSG=$(conf_lookup PMSG)
  EFF_LINES=$(clamp_int "$RAW_LINES" 200 20000 20000)
  say "  BUFFERS     raw='${RAW_BUF:-(absent)}' (whitelisted by name; unknown names are dropped)"
  say "  BOOT_LOGCAT raw='${RAW_DUMP:-(absent)}' -> $([ "$RAW_DUMP" = "0" ] && echo "0 (no boot snapshot: zero logcat storage)" || echo "1 (one snapshot per boot)")"
  say "  BOOT_LINES  raw='${RAW_LINES:-(absent)}' -> effective ${EFF_LINES} lines"
  say "  PMSG        raw='${RAW_PMSG:-(absent)}' -> $([ "$RAW_PMSG" = "0" ] && echo "0 (pmsg files skipped)" || echo "1 (pmsg files copied)")"
  say "  FORMAT      raw='$(conf_lookup FORMAT)'"
  say "  note: this version has no continuous capture, so ROTATE_KB / ROTATE_COUNT"
  IGNORED=0
  while IFS= read -r LINE; do
    case "$LINE" in
      ''|'#'*) continue ;;
    esac
    # `KEY*=*` matches "KEY=..." and "KEY = ..."; the earlier version used a pattern that
    # required a space before the `=`, so every correct line was reported as ignored.
    case "$LINE" in
      BUFFERS*=*|BOOT_LOGCAT*=*|BOOT_LINES*=*|FORMAT*=*|PMSG*=*) continue ;;
    esac
    IGNORED=$((IGNORED + 1))
    say "  IGNORED (not a KEY=value line): $LINE"
  done < "$CONF"
  if [ "$IGNORED" -gt 0 ]; then
    say "  $IGNORED line(s) ignored. The file is read, never executed, so this is not dangerous;"
    say "  but if you pasted shell code there, it is doing nothing at all."
  else
    say "  no ignored lines"
  fi
  return 0
}

cmd_sizes() {
  for d in boot kernel live logs run; do
    if [ -d "$OUT/$d" ]; then
      say "$(du -sk "$OUT/$d" 2>/dev/null | cut -f1) KB  $OUT/$d  ($(ls -1 "$OUT/$d" 2>/dev/null | wc -l) entries)"
    fi
  done
  FREE=$(df -Pk /data 2>/dev/null | awk 'NR==2 {print $4}')
  [ -n "$FREE" ] && say "free on /data: $FREE KB"
  return 0
}

# Deletes everything this module collected. config.env is kept on purpose: it is a setting, not
# data, and re-creating it would silently reset the user's choices.
cmd_purge() {
  cmd_stop
  GONE=0
  for d in boot kernel live; do
    usable_dir "$OUT/$d" || continue
    for f in "$OUT/$d"/*; do
      [ -e "$f" ] || continue
      [ -L "$f" ] && continue
      [ -d "$f" ] && continue
      rm -f "$f" 2>/dev/null && GONE=$((GONE + 1))
    done
  done
  say "purged $GONE file(s); config.env kept"
  cmd_sizes
  return 0
}

case "$1" in
  status) cmd_status ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  snapshot) sh "$MODDIR/boot-completed.sh"; say "snapshot requested" ;;
  confcheck) cmd_confcheck ;;
  sizes) cmd_sizes ;;
  purge) cmd_purge ;;
  disable)
    touch "$OUT/.disabled" 2>/dev/null
    cmd_stop
    say "disabled; re-enable with: sh ctl.sh enable"
    ;;
  enable)
    rm -f "$OUT/.disabled" 2>/dev/null
    say "enabled; start now with: sh ctl.sh start"
    ;;
  *)
    say "usage: sh ctl.sh status|start|stop|restart|snapshot|confcheck|sizes|purge|disable|enable"
    exit 1
    ;;
esac

exit 0
