#!/system/bin/sh
# KernelSU / Magisk installer hook.
#
# Android zips do not carry unix permissions, so the installer is the only place that can mark the
# boot scripts executable. Every helper used here is optional: this file must not be able to fail an
# install, so each one is checked before use and falls back to chmod.
#
# Order matters, and the first version got it wrong: `set_perm_recursive` ran *after* the individual
# `set_perm` calls and reset the scripts to 0644, so the intent ("scripts executable, data files
# not") was never achieved. Recursive first, then the exceptions.

SKIPUNZIP=0

# The module keeps its data 0700/0600 on purpose; see service.sh. Set here as well so anything the
# installer creates inherits it.
umask 077

say() {
  if command -v ui_print >/dev/null 2>&1; then
    ui_print "$1"
  else
    echo "$1"
  fi
}

say "- Droidlog Boot Log"
say "- Writes only to /data/adb/droidlog"
say "- No system partition, boot image or property is touched"
say "- It captures: pstore kernel logs (the previous crash), this boot's dmesg,"
say "  and one boot-time logcat snapshot. There is NO continuous capture:"
say "  live logs are collected by the desktop app instead. The snapshot and"
say "  pstore kernel logs contain app launches and package names; only root can read them."
say "- The action button (the play icon) deletes the collected logs and keeps config.env."
say "- Uninstalling stops the capture but KEEPS the collected logs;"
say "  run 'sh ctl.sh purge' first if you want them gone."

# Resolve the module directory: MODPATH is set by the installer, otherwise fall back to this
# script's own directory.
DIR=${MODPATH:-${0%/*}}
[ -n "$DIR" ] || DIR=.

# 1) Everything is 0755/0644 by default...
if command -v set_perm_recursive >/dev/null 2>&1; then
  set_perm_recursive "$DIR" 0 0 0755 0644
else
  chmod 0755 "$DIR" 2>/dev/null
fi

# 2) ...and then the entry points are made executable, which is the point of this file.
for SCRIPT in post-fs-data.sh service.sh boot-completed.sh ctl.sh action.sh uninstall.sh; do
  if command -v set_perm >/dev/null 2>&1; then
    set_perm "$DIR/$SCRIPT" 0 0 0755
  else
    chmod 0755 "$DIR/$SCRIPT" 2>/dev/null
  fi
done

# 3) The data directory this module owns, 0700, created here so the first boot has somewhere to
#    write even if it never reaches the late_start service.
mkdir -p /data/adb/droidlog/boot /data/adb/droidlog/kernel \
  /data/adb/droidlog/logs /data/adb/droidlog/run 2>/dev/null
chmod 700 /data/adb/droidlog /data/adb/droidlog/boot /data/adb/droidlog/kernel \
  /data/adb/droidlog/logs /data/adb/droidlog/run 2>/dev/null

say "- Installed. Restart the device, then check with: sh /data/adb/modules/droidlog_bootlog/ctl.sh status"
