# 03 — Session: operator in the graphical session, OSD, GPUI app

Names and process boundaries: 15-layout.

## Split of responsibilities

| Process | Runs as | Owns in the session |
| --- | --- | --- |
| `legionctl-system` | root, system service | hardware state, input devices, policy; emits key and state signals on `org.legionctl.System1` |
| `legionctl-operator` | user service | Mutter DisplayConfig calls, OSD requests, notifications for approvals; active only while a graphical session exists (logind) |
| GNOME Shell extension `legionctl-osd@legionctl` | inside gnome-shell | OSD drawing and animation |
| `legionctl-app` | user, on demand | GPUI desktop app |

`legionctl-system` never talks to the session bus. The operator subscribes to its signals and calls the session APIs.

## OSD

GNOME restricts `org.gnome.Shell.ShowOSD` to its own settings daemon (`AccessDenied`, measured 2026-10-12) and has no
layer-shell, so the OSD is a GNOME Shell extension. It is the only OSD path. When the extension is not enabled,
`legionctl doctor` reports it and the app offers to enable it.

The extension draws its own OSD (St widgets on the Clutter stage), not the stock `osdWindowManager` box, so each
key press can have its own animation. It exports one session D-Bus method, `Show(kind, state, level)`, that the
operator calls.

| Press or event | Animation |
| --- | --- |
| Fn+Q (thermal mode) | a row of the mode icons (Quiet, Balanced, Performance, Max power); the strip slides to the new mode and the selected icon scales up with a glow in the mode color |
| Fn+R (panel refresh) | the refresh number counts from the old rate to the new rate; a sweep line crosses the panel outline at the new rate |
| Fn+Space (keyboard light) | the key glyph fills from level to level |
| Fn+Esc (Fn lock) | a lock that closes or opens with a short spring |
| AC plugged / unplugged | a plug that slides in or out; the new power limit counts up or down |
| Firmware limit change, battery threshold | a level bar that eases to the new value |
| Conflict (another process changed our setting) | a short shake and the name of that process |

Rules:

- Ease with Clutter (`actor.ease`, ease-out-quad and spring curves), 150–250 ms in, 1.2 s visible, 200 ms out.
- A new press while the OSD shows animates from the current state; it does not restart the OSD. Fn+R bursts are
  debounced in `legionctl-system`, so one press gives one animation.
- Honor GNOME "Reduce animation" (`org.gnome.desktop.interface enable-animations`): no motion, only a fade.
- Colors and fonts from the shell theme (light and dark); position and size scale with the monitor's scale factor
  and show on the monitor with the pointer.
- The extension follows GNOME's extension API for each release (GNOME 51 today).

## GPUI desktop app

`legionctl-app` is the only graphical UI. It is a client of the operator API (15-layout), like the CLI and TUI.

- Live view: package and GPU power, temperatures, clocks, fan RPM, throttle reasons, active layers.
- Configset editor with plan preview and inline validation errors.
- Tuning wizard: run the sweep, show the knee for each scenario, write a generated layer for review.
- Windows panel: hazards (fast startup, RTC), reboot to Windows, start and stop the Windows VM.
- Agents: sessions, worktrees, plans, approvals, usage.

GPUI has no stable API and ships with Zed, so the app pins a git revision. Every view uses operations from the
registry, so the CLI can do everything that the app can do.

Desktop entry `legionctl-app.desktop`. When the operator is not running, the app shows that state and a button
that starts `legionctl-operator.service`.

## Display actions

- Fn+R: `legionctl-system` emits `Key(FnR)`; the operator toggles the internal panel between the highest rate and
  about 60 Hz at the current resolution, keeps the VRR state, and uses method TEMPORARY (no confirmation dialog).
- `display.internal_refresh` from the active layer (for example 60 Hz on battery) is applied by the operator when
  the power source changes; a manual Fn+R wins until the next change (same follow rule as Fn+Q).
- Primary GPU: the operator reports which card drives the active outputs; `legionctl-system` writes the udev tag
  for the next login, and the OSD says that a new login applies it.
