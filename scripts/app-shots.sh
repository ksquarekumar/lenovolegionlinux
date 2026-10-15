#!/bin/bash
# Screenshots of legionctl-app's pages (the visual check of the app, todo-127): a private headless GNOME Shell on its
# own D-Bus session and its own settings (a plain background, no extensions, your dark or light choice), the app on
# each page, a screenshot through the desktop portal (GNOME refuses the shell's own Screenshot call), cut to the
# window. The app talks to an operator of its own build (the binary next to it) on a private socket, with a home of
# its own that holds a copy of your operator.db (threads, bench runs); it reads the machine and clicks nothing, so
# nothing on the machine, in your home or in your desktop settings changes.
# usage: scripts/app-shots.sh <folder> [app binary] [pages...]; APP_SHOTS_SCHEME='prefer-light' for the light look
set -eu
out=$(realpath -m "${1:?usage: scripts/app-shots.sh <folder> [app binary] [pages...]}")
app=$(realpath "${2:-target/release/legionctl-app}")
shift $(($# < 2 ? $# : 2))
pages=("$@")
[ ${#pages[@]} -gt 0 ] || pages=(now gpu health bench resources)
if [ -z "${APP_SHOTS_WORK:-}" ]; then
  work=$(mktemp -d /var/tmp/app-shots.XXXXXX)
  mkdir -p "$work/config/glib-2.0/settings"
  scheme=${APP_SHOTS_SCHEME:-$(gsettings get org.gnome.desktop.interface color-scheme)}
  printf "[org/gnome/desktop/interface]\ncolor-scheme=%s\n[org/gnome/desktop/background]\n%s\n" "$scheme" \
    "picture-options='none'
primary-color='#3d3846'
color-shading-type='solid'" >"$work/config/glib-2.0/settings/keyfile"
  # a private session bus and settings: the shell and its portal never touch your desktop or its dconf
  exec env APP_SHOTS_WORK="$work" GSETTINGS_BACKEND=keyfile XDG_CONFIG_HOME="$work/config" \
    dbus-run-session -- "$0" "$out" "$app" "${pages[@]}"
fi
work=$APP_SHOTS_WORK
mkdir -p "$out" "$work/run/legionctl" "$work/home/.local/state/legionctl"
sqlite3 "$HOME/.local/state/legionctl/operator.db" ".backup '$work/home/.local/state/legionctl/operator.db'"
systemd-socket-activate -l "$work/run/legionctl/api.sock" --setenv=HOME="$work/home" \
  --setenv=XDG_RUNTIME_DIR="$work/run" --setenv=PATH="$PATH" \
  --setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/bus "$(dirname "$app")/legionctl-operator" \
  >"$out/operator.log" 2>&1 &
operator=$!
display=app-shots-$$
# the app's runtime folder is the private operator's; it finds the shell by its full path
wayland=$XDG_RUNTIME_DIR/$display
gnome-shell --headless --virtual-monitor 2560x1600 --wayland-display "$display" >"$out/shell.log" 2>&1 &
shell=$!
trap 'kill $shell $operator 2>/dev/null; wait 2>/dev/null; rm -rf "$work"' EXIT
for _ in $(seq 40); do
  [ -S "$wayland" ] && break
  sleep 0.5
done
[ -S "$wayland" ] || {
  echo "app-shots: the headless shell did not start; see $out/shell.log" >&2
  exit 1
}
sleep 3
# the shell starts in its overview: Escape through Mutter's remote desktop interface shows the desktop
python3 -I - <<'EOF'
import time
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
def call(path, iface, method, args=None):
    return bus.call_sync("org.gnome.Mutter.RemoteDesktop", path, iface, method, args, None, 0, -1, None)
session = call("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession").unpack()[0]
call(session, "org.gnome.Mutter.RemoteDesktop.Session", "Start")
for pressed in (True, False):
    call(session, "org.gnome.Mutter.RemoteDesktop.Session", "NotifyKeyboardKeysym",
         GLib.Variant("(ub)", (0xff1b, pressed)))
time.sleep(1)
call(session, "org.gnome.Mutter.RemoteDesktop.Session", "Stop")
EOF
sleep 1
# shoot <file>: the whole screen through the portal
shoot() {
  python3 -I - "$1" <<'EOF'
import shutil, sys, urllib.parse
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
loop, out = GLib.MainLoop(), {}
token = "appshots%d" % GLib.random_int()
sender = bus.get_unique_name()[1:].replace(".", "_")
handle = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
def response(_c, _s, _p, _i, _sig, params):
    code, result = params.unpack()
    out["code"], out["uri"] = code, result.get("uri")
    loop.quit()
bus.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", handle,
                     None, 0, response)
bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
              "org.freedesktop.portal.Screenshot", "Screenshot",
              GLib.Variant("(sa{sv})", ("", {"handle_token": GLib.Variant("s", token),
                                             "interactive": GLib.Variant("b", False)})),
              None, 0, -1, None)
GLib.timeout_add_seconds(60, loop.quit)
loop.run()
if not out.get("uri"):
    sys.exit(f"app-shots: no screenshot: {out}")
shutil.move(urllib.parse.unquote(out["uri"].removeprefix("file://")), sys.argv[1])
EOF
}
shoot "$work/desktop.png"
for page in "${pages[@]}"; do
  XDG_RUNTIME_DIR=$work/run WAYLAND_DISPLAY=$wayland DISPLAY='' "$app" "$page" >"$out/$page.log" 2>&1 &
  pid=$!
  # the first answers of the operator (the first page also starts it), and the portal's dark/light choice
  sleep "${wait:-15}"
  wait=7
  shoot "$work/$page.png"
  kill $pid
  wait $pid 2>/dev/null || true
  # the window alone: what differs from the empty desktop, below the top bar and its clock
  window=$(magick "$work/$page.png" "$work/desktop.png" -compose difference -composite \
    -fill black -draw 'rectangle 0,0 10000,40' -threshold 2% -trim -format '%wx%h%X%Y' info:)
  magick "$work/$page.png" -crop "$window" +repage "$out/$page.png"
  echo "$out/$page.png"
done
