# 21 — Greeter: greetd and legionctl's login screen in place of GDM

Decided by the user 2026-10-14 (todo-83). Built 2026-10-14, part 1. The MUX mode with a restart is not in this part:
it waits for todo-17, and the screen shows nothing for it.

## What it does

- greetd starts `legionctl-greeter` in `cage` (a kiosk Wayland compositor) as greetd's user `greeter`.
- You log in with the keyboard or the mouse: type the user name, push Enter, type the answer, push Enter.
- PAM behind greetd does the password and the fingerprint. The screen shows each PAM message as the prompt. A secret
  prompt hides the text. An info message (for example "Place your finger on the fingerprint reader") shows while PAM
  waits.
- You choose the session from `/usr/share/wayland-sessions`. GNOME is first and is the default. The screen does not
  show sessions with `Hidden=true` or `NoDisplay=true`.
- The screen selects the session that the user started last when the user name is entered, unless you chose one on
  this screen (todo-150). It keeps one `<user> <session>` line per user in `/var/lib/legionctl-greeter/last-sessions`;
  `greeter install` makes that folder for the user `greeter` (its home is `/`), and `greeter remove` keeps it. The
  greeter writes the line after greetd started the session; a failed write goes to greetd's journal, and the session
  starts.
- X11 sessions (`/usr/share/xsessions`) are listed last as "not supported" and cannot be chosen, with the reason: the
  greeter runs in cage (Wayland) and starts no X server; log in to them through GDM.
- Before login, for the user name that you typed, the screen changes these through the daemon:

| Row | Buttons | Daemon method (polkit action) |
| --- | --- | --- |
| Windows VM | Start, Stop | `GreeterAct(user, vm-start / vm-stop)` (`org.legionctl.greeter.act`) |
| Windows disk | Attach to Linux, Detach | `GreeterAct(user, disk-attach / disk-detach)` |
| NVIDIA GPU | Linux, Free for the VM | `GreeterAct(user, gpu-linux / gpu-none)` |
| Desktop GPU | Intel, NVIDIA, Auto | `SetPrimaryGpu(choice, false)` (`org.legionctl.gpu.primary`) |

How each action works:

- **VM start and stop**: the daemon starts `user@<uid>.service` (the user's service manager), then
  `systemctl --user -M <user>@ start --no-block legionctl-windows-vm.service`. Stop stops that unit: the unit sends
  the power button and waits for Windows (note 05).
- **VM start needs autostart**: the VM's runner asks polkit as the user. Before login, no polkit agent can ask a person.
  Thus the daemon refuses a start when the user's autostart rule (note 05) does not exist.
- **No autostart** (P-1, EV-review-08, 2026-10-16): the greeter changes nothing that lasts. The daemon refuses
  `autostart-off / -login / -boot` from the greeter; each user sets autostart after login with `legionctl windows vm
  autostart` (the user's own approval). The rule grants only the user's local, active session (P-2), so a start
  before login gets no grant.
- **Windows disk**: `Detach` stops the automount and mount units of the fstab entries on the Windows disk, as a VM
  start does. `Attach to Linux` starts the automounts again. The daemon refuses both while the Windows VM runs.
- After login, `legionctl windows disk attach|detach` (D-Bus `WindowsDisk`, polkit `org.legionctl.windows.vm`) runs
  the same function (note 22).
- **NVIDIA GPU**: `Free for the VM` is `gpu give nvidia none` for the typed user (`/dev/vfio/<group>` goes to that
  user). `Linux` gives the GPU back. To give the GPU to a running Windows, use `legionctl gpu give nvidia windows`
  after login (QMP is in the user's session).
- **Desktop GPU**: the same as `legionctl gpu primary`. It has an effect at the next login.

The daemon acts only for a person's account (uid 1000 to 65533 in `/etc/passwd`), never for a system account.

## The protocol (greetd-ipc(7))

Each message is one frame on `$GREETD_SOCK`: a u32 length in native byte order, then JSON.

1. `{"type":"create_session","username":U}`.
2. greetd answers `auth_message` (`visible`, `secret`, `info`, `error`), `success` or `error`.
3. The screen answers each message with `{"type":"post_auth_message_response","response":R}`. R is the text for
   `visible` and `secret`. R is `null` for `info` and `error`.
4. On `success`: `{"type":"start_session","cmd":Exec,"env":["XDG_SESSION_TYPE=wayland","XDG_SESSION_DESKTOP=<id>",
   "XDG_CURRENT_DESKTOP=<DesktopNames>"]}`. Then the greeter stops, and greetd starts the session.
5. On `error` (for example a wrong password): greetd keeps the failed session. The screen sends `cancel_session`, shows
   the reason, and starts a new login of the same user. `Cancel` also sends `cancel_session`.

## The polkit rule

The package installs `/usr/share/polkit-1/rules.d/50-legionctl-greeter.rules`. It lets the user `greeter` do two
actions without a prompt: `org.legionctl.greeter.act` and `org.legionctl.gpu.primary`. It does not let other users do
them, and it does not let `greeter` do other actions. `org.legionctl.greeter.act` is `no` for all other subjects, so
only the login screen can do it.

Risk: a person at the keyboard can do these actions before login without a password. The user accepted this when
deciding the feature. The daemon limits it to the actions above and to people's accounts.

## Install and remove

`legionctl greeter status|install|remove`. Install and remove go through the daemon's `Greeter(action)` (polkit
`org.legionctl.greeter`, always a password, no kept authorization). The operator makes the plan first, so a refusal
comes before the polkit dialog. `--dry-run` shows the plan.

Install refuses when:

- greetd, cage or `legionctl-greeter` is not installed (the message names each, with its package);
- the display manager is not `gdm.service` (legionctl replaces only GDM, because GDM is the way back);
- legionctl's greeter is installed already (a second install must not overwrite the kept file).

Install does these steps in this order:

1. It keeps `/etc/greetd/config.toml` as `/etc/greetd/config.toml.before`.
2. It writes `/etc/greetd/config.toml` (first line `# legionctl greeter`): VT 1, `cage -s -- legionctl-greeter` as
   `greeter`.
3. `systemctl disable gdm.service`. GDM stays installed.
4. `systemctl enable greetd.service`. The `display-manager.service` alias is one link, so GDM must go first.

The change has an effect at the next start.

## The way back to GDM

- `legionctl greeter remove`: it puts the kept file back (or removes ours when there was no file), then
  `systemctl disable greetd.service` and `systemctl enable gdm.service`. It refuses when GDM is not installed.
- When the package is removed, its `pre_remove` runs `legionctl-system --greeter-remove`: the same steps, so the
  computer keeps a login screen. pacman ignores a failed `pre_remove`, so before that, the hook
  `05-legionctl-remove.hook` (`PreTransaction`, `AbortOnFail`) runs `legionctl-system --remove-check`. It stops the
  removal when the greeter cannot be removed (GDM is not installed: greetd would start a deleted greeter) or when the
  boot takeover is installed (no hook would make its images again after a kernel update). The message names the
  command to run first: `legionctl greeter remove` or `legionctl boot remove`.
- When the greeter does not start: switch to a text console (Ctrl+Alt+F2), log in, and run
  `sudo systemctl disable greetd.service && sudo systemctl enable gdm.service`, then restart.

## Tests (e2e)

- The greetd client against a stand-in greetd with the real framing and greetd's session rules: a password login
  (the start request with the command and environment), a wrong password and a new try, the fingerprint message then
  the password, a cancelled login.
- The screen through gpui-kit's test support: the session list (GNOME first, hidden sessions not shown), a click on
  another session, the user name, Enter, the password, Enter: greetd gets `start_session` with that session's command.
- Install and remove through the operator and the real daemon on the recorded machine: refused without cage (nothing
  changed); the old file kept byte for byte; GDM disabled before greetd is enabled; a second install refused; remove
  puts the old file and GDM back.
- `the_package_removal_stops_while_the_boot_takeover_or_a_greeter_without_gdm_is_installed`: the hook's `Exec` line
  on the recorded machine: it passes with nothing installed; it fails with the boot entry or with the greeter and no
  GDM, and names the command to run first; it passes again with GDM.
- `the_greeter_cannot_write_a_lasting_autostart_rule`: the greeter's autostart actions are refused and write no rule; a
  VM start without the user's rule is refused.
