# 03 — Session: agent, OSD, GPUI control center

## Split of responsibilities

| Process | Runs as | Owns |
| --- | --- | --- |
| `legiond` | root, system service | hardware state, input devices, policy, D-Bus API |
| `legion-agent` | user, `graphical-session.target` | anything that needs the session: Mutter DisplayConfig, OSD rendering, notifications |
| `legion-center` | user, on demand | GPUI control center |
| GNOME Shell extension | inside gnome-shell | native OSD drawing |

The agent subscribes to `org.legion.Control1` signals on the system bus and calls session APIs. legiond never
talks to the session bus directly.

## OSD

GNOME restricts `org.gnome.Shell.ShowOSD` to its own settings daemon (`AccessDenied` measured on 2026-10-12)
and does not implement layer-shell, so a third-party process cannot draw an overlay. Backends, chosen at runtime:

1. **GNOME Shell extension** `legion-osd@legion` (preferred on GNOME). Exports a session D-Bus method
   `Show(icon, label, level)` that calls `Main.osdWindowManager.show(-1, icon, label, level)`: the same OSD as
   volume/brightness, including a level bar (useful for firmware limits or battery threshold). About 50 lines of
   JavaScript; must track GNOME's extension API each release (GNOME 51 today).
2. **layer-shell overlay** for compositors that support it (KDE, Hyprland, Sway): GPUI or a small
   `gtk4-layer-shell` surface.
3. **Notifications** (today's `legion-osd`): transient, replaced per topic. Works everywhere.

OSD events: thermal mode (Fn+Q, power source), Fn lock, refresh rate, conservation mode, camera power, AC
plug/unplug with the resulting configset, conflicts (another process fighting a knob), tuning progress.

## GPUI control center

GPUI (Zed's UI framework) fits a desktop app, not an overlay on GNOME. `legion-center` scope:

- live view: package/GPU power, temperatures, clocks, fan RPM (once validated), throttle reasons, active layers;
- configset editor with `plan` preview and validation errors inline;
- tuning wizard: run the sweep, show the knee per scenario, write a generated layer for review;
- Windows panel: hazards (fast startup, RTC), reboot-to-Windows, start/stop the Windows VM.

Risks: GPUI's API is not stable and is published with Zed, so pin a git revision and keep the center optional
(`gpui` feature). Everything it shows must also be reachable through `legionctl` so the GUI is never required.

## Web UI and desktop app

Requirement (2026-10-12): a web UI (WebAssembly) and/or a desktop app, written React-style in Rust, available
whenever the package is installed and legiond is running.

| Option | Web (wasm) | Desktop | Notes |
| --- | --- | --- | --- |
| **Dioxus** (recommended) | yes | yes (webview), native renderer maturing | React-like components and hooks in Rust; one codebase for both targets |
| Leptos | yes | via Tauri | Fine-grained signals; desktop needs a second shell |
| GPUI | no | yes, native | Best native performance; no web target; unstable API (see above) |
| React (TypeScript) + Rust backend | yes | via Tauri | Largest ecosystem, but a second language in the repo |

Recommendation: **Dioxus** for a single UI codebase (`legion-ui` crate) that builds both the wasm web UI and the
desktop app; keep GPUI `legion-center` optional for a native-first variant if Dioxus desktop falls short.

### Serving and availability

- The web UI is served by **legion-agent** (user session), not by root legiond. The agent exposes HTTP + a
  WebSocket on `127.0.0.1` with a random port and per-session token, written to `$XDG_RUNTIME_DIR/legion/web.json`
  (mode 0600); the desktop app and `legionctl ui` read it to open the right URL.
- Every mutation goes from the agent to legiond over D-Bus **as the user**, so polkit decides; the web layer
  never holds privileges. Origin check and token on every request; no remote binding unless explicitly
  configured.
- Lifecycle: `legion-agent` is `PartOf=`/`After=` the session and only starts the web server once
  `org.legion.Control1` is on the system bus; it stops serving when legiond disappears (name-owner-changed), so
  the UI is available exactly when the package is installed and the daemon is running.
- Desktop entry `legion-ui.desktop` with `TryExec=` the binary; the app shows a clear "daemon not running"
  state with a button that triggers `systemctl start legiond` through polkit.
- Live data: legiond `StateChanged`/sensor signals → agent → WebSocket push; no polling from the browser.

## Display actions in the agent

- Fn+R: `legiond` emits `Key(FnR)` → agent toggles the internal panel between highest and ~60 Hz at the current
  resolution, keeping VRR state, with method TEMPORARY (no confirmation dialog).
- `display.internal_refresh` from the active layer (for example 60 Hz on battery) is applied by the agent on
  power-source changes; a manual Fn+R wins until the next change (same follow semantics as Fn+Q).
- Primary GPU recommendation: the agent reports which card drives the active outputs; legiond writes the udev
  tag for the next login and the OSD says that a re-login applies it.
