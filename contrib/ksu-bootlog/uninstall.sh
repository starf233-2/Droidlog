#!/system/bin/sh
# Called when the module is removed.
#
# It stops any writer left by an older version of this module and removes only this module's run
# directory. The collected logs in /data/adb/droidlog are deliberately LEFT IN PLACE: they are the
# evidence this module exists to capture, and an uninstall is a bad moment to destroy data someone
# may still need. Because that choice means the files outlive the module (and therefore outlive its
# retention, which no longer runs), the installer and the README both say so, and `ctl.sh purge` is
# the supported way to remove them:
#
#   sh /data/adb/modules/droidlog_bootlog/ctl.sh purge
#
# A root shell can of course also do `rm -rf /data/adb/droidlog`.
#
# There is nothing to undo on the system side: nothing outside /data/adb was ever written.

MODDIR=${0%/*}
OUT=/data/adb/droidlog

umask 077

# True only for a real directory that is not a symlink; see service.sh.
usable_dir() {
  [ -n "$1" ] || return 1
  [ -L "$1" ] && return 1
  [ -d "$1" ] || return 1
  return 0
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

for p in "$OUT"/run/*.pid; do
  [ -f "$p" ] || continue
  PID=$(cat "$p" 2>/dev/null)
  case "$PID" in
    ''|*[!0-9]*) continue ;;
  esac
  if is_ours "$PID"; then
    kill "$PID" 2>/dev/null
    sleep 1
    # Re-checked before the harder signal, as everywhere else.
    if is_ours "$PID"; then
      kill -9 "$PID" 2>/dev/null
    fi
  fi
done

usable_dir "$OUT/run" && rm -rf "$OUT/run" 2>/dev/null

exit 0
