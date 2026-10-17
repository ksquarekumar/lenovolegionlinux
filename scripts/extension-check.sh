#!/bin/bash
# Is the GNOME Shell extension of this checkout ACTIVE on the installed GNOME Shell? A private headless shell on its
# own D-Bus session, settings and data folder enables only this extension, and gnome-extensions info gives its state
# (EV-review-11 U-7: on GNOME 51 an unknown addTopChrome parameter made enable() throw, State ERROR, and no type check
# saw it: @girs/gnome-shell has no GNOME 51 types). Nothing on your desktop or in its settings changes.
# usage: scripts/extension-check.sh
set -eu
uuid=legionctl-osd@legionctl
src=$(realpath "gnome-extension/$uuid")
if [ -z "${EXTENSION_CHECK_WORK:-}" ]; then
  work=$(mktemp -d /var/tmp/extension-check.XXXXXX)
  mkdir -p "$work/config/glib-2.0/settings" "$work/data/gnome-shell/extensions"
  ln -s "$src" "$work/data/gnome-shell/extensions/$uuid"
  printf "[org/gnome/shell]\nenabled-extensions=['%s']\ndisable-user-extensions=false\n" "$uuid" \
    >"$work/config/glib-2.0/settings/keyfile"
  trap 'rm -rf "$work"' EXIT
  EXTENSION_CHECK_WORK="$work" GSETTINGS_BACKEND=keyfile XDG_CONFIG_HOME="$work/config" \
    XDG_DATA_HOME="$work/data" dbus-run-session -- "$0"
  exit
fi
work=$EXTENSION_CHECK_WORK
display=extension-check-$$
gnome-shell --headless --virtual-monitor 1280x720 --wayland-display "$display" >"$work/shell.log" 2>&1 &
shell=$!
trap 'kill $shell 2>/dev/null; wait 2>/dev/null' EXIT
info=
for _ in $(seq 60); do
  info=$(gnome-extensions info "$uuid" 2>/dev/null || true)
  case $info in *"State: ACTIVE"* | *"State: ERROR"* | *"State: OUT OF DATE"*) break ;; esac
  sleep 0.5
done
echo "$info"
case $info in
  *"Path: $work/data/gnome-shell/extensions/$uuid"*"State: ACTIVE"*) echo "extension-check: $uuid is ACTIVE on $(gnome-shell --version)" ;;
  *)
    grep -i -A3 "$uuid\|extension" "$work/shell.log" | head -20 >&2
    echo "extension-check: $uuid is not ACTIVE on $(gnome-shell --version)" >&2
    exit 1
    ;;
esac
