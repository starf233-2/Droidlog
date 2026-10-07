#!/system/bin/sh
# Droidlog Boot Log: the KernelSU action button.
#
# KernelSU runs this file when the user taps the action (the play button at the module's lower
# left). Its job here is exactly one thing: delete the logs this module has collected, so the
# button is a "clear the data" button and nothing more.
#
# What it touches, and what it does not:
#
#   * It deletes regular files inside /data/adb/droidlog/boot, /kernel and /live. Nothing outside
#     that directory is ever written or removed -- no system path, no module file.
#   * config.env is kept. It is a setting, not a log, and re-creating it would silently reset the
#     user's choices; `sizes` and `confcheck` still work afterwards.
#   * Symbolic links are skipped rather than followed, and so are subdirectories: a planted link
#     cannot turn "delete my logs" into "delete something else".
#   * Any writer left by an older version is stopped first. Deleting logcat's file while it is open
#     would free no space at all (the process keeps writing to the unlinked inode) and the file
#     would come back the moment it rotates. This version runs no writer of its own.
#
# Output goes to KernelSU's action log, so it is worth printing what happened.

MODDIR=${0%/*}
OUT=/data/adb/droidlog

umask 077

say() { echo "$1"; }

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

stop_writer() {
  for p in "$OUT"/run/*.pid; do
    [ -f "$p" ] || continue
    PID=$(cat "$p" 2>/dev/null)
    if is_ours "$PID"; then
      kill "$PID" 2>/dev/null
      sleep 1
      if is_ours "$PID"; then
        kill -9 "$PID" 2>/dev/null
      fi
      say "stopped capture pid $PID"
    fi
    rm -f "$p" 2>/dev/null
  done
  return 0
}

say "Droidlog Boot Log: clearing collected logs"
say "only /data/adb/droidlog is touched; config.env is kept"

if ! usable_dir "$OUT"; then
  say "nothing to do: $OUT does not exist"
  exit 0
fi

BEFORE=$(du -sk "$OUT" 2>/dev/null | cut -f1)
[ -n "$BEFORE" ] || BEFORE=0

stop_writer

GONE=0
for d in boot kernel live; do
  usable_dir "$OUT/$d" || continue
  for f in "$OUT/$d"/*; do
    [ -e "$f" ] || continue
    [ -L "$f" ] && continue
    [ -d "$f" ] && continue
    rm -f "$f" 2>/dev/null && GONE=$((GONE + 1))
  done
  say "cleared $OUT/$d"
done

# The module log is a log too, but it is truncated rather than removed so the next line can still
# say that the action ran.
: > "$OUT/logs/module.log" 2>/dev/null
say "truncated $OUT/logs/module.log"

AFTER=$(du -sk "$OUT" 2>/dev/null | cut -f1)
[ -n "$AFTER" ] || AFTER=0
say "removed $GONE file(s); $((BEFORE - AFTER)) KB freed"

# Re-run the housekeeping: the button clears the logs, it does not disable the module. `disable` and
# `enable` are in ctl.sh for that.
if [ -f "$MODDIR/disable" ] || [ -f "$OUT/.disabled" ]; then
  say "module is disabled; not restarting the capture"
else
  sh "$MODDIR/service.sh" 2>/dev/null
  say "housekeeping re-run"
fi

exit 0
