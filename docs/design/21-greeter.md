# 21 — Greeter: greetd and legionctl's login screen in place of GDM

Decided by the user 2026-10-14 (todo-83). Built 2026-10-14, part 1. The MUX mode with a restart is not in this part:
it waits for todo-17, and the screen shows nothing for it.

## What it does

- greetd starts `legionctl-greeter` in `cage -s -m last` (a kiosk Wayland compositor) as greetd's user `greeter`,
  through `systemd-cat -t legionctl-greeter`: the greeter's and cage's messages go to the journal
  (`journalctl -t legionctl-greeter`). `-m last` shows the screen on one monitor: with two monitors, cage's default
  (`extend`) spans one window over both, and the login was at the edge of one monitor (boot of 2026-10-17).
- The greeter reads only its own fonts (`FONTCONFIG_FILE=/usr/share/legionctl/greeter/fonts.conf`: links to five
  noto-fonts files) and has no `DISPLAY`. Boot of 2026-10-17 (boot-handover): GPUI parsed every system font (1543
  faces, 2.4 GB) before its first frame, 3.5 s; and NVIDIA's Vulkan driver connected to `DISPLAY`, so cage started
  Xwayland before the first frame. The package needs noto-fonts for the greeter (install and upgrade check it).
- The boot splash stays until the greeter shows (todo-200 item 1, as GDM does). legionctl writes its own
  `/etc/systemd/system/greetd.service` (greetd's package unit has `After=plymouth-quit-wait.service`, which a drop-in
  cannot take away): `Conflicts=` and `After=plymouth-quit.service`, `ExecStartPre=-plymouth deactivate` (plymouth
  lets go of the screen, the splash stays on it); after its first frame the greeter starts
  `legionctl-splash-quit.service` (`plymouth quit --retain-splash`; the polkit rule allows only that start to
  `greeter`). Before boot b of 2026-10-17, plymouth quit at 11.45 s and cage drew at 12.54 s: the console frame
  showed between.
- The NVIDIA driver loads beside the boot, not before it. Boot b: systemd-modules-load loaded nvidia, nvidia_modeset,
  nvidia_uvm and nvidia_drm for 3.29 s, and sysinit.target (so greetd) waited. udev loads the driver by itself
  (modalias of nvidia and nvidia_drm; nvidia-utils' 60-nvidia.rules loads nvidia_uvm when nvidia binds). legionctl
  empties `/etc/modules-load.d/nvidia-580xx-utils.conf`, which hides the list of nvidia-580xx-utils of the same name
  in `/usr/lib`; greetd's unit runs `udevadm settle --timeout=10` first, so the monitors on the NVIDIA GPU are there
  when cage starts.
- The daemon writes only two files for this (user decision 2026-10-18): `/etc/systemd/system/greetd.service` and
  `/etc/modules-load.d/nvidia-580xx-utils.conf`. The package owns both (backup files) with greetd's own unit and
  nvidia-580xx-utils' list in them, so they change nothing until `greeter install`; `remove` writes them back; the
  daemon's `ReadWritePaths` has the two files, not their folders.
- A list in `/etc` of another owner (here `nvidia.conf`, of no package) is not changed. No package file can win
  over it: `/etc` is the first place systemd-modules-load reads, and a file of the same name in `/etc` is that file.
  So it is the user's one step: install and upgrade print it, and doctor (Health) has it as a finding with the
  command (`sudo rm <file>`) while legionctl's greeter is the login screen.
- The screen is dark and shows little text (todo-150, 2026-10-17): the people of the computer as avatars (uid 1000 to
  65533 with a login shell), the session chips, and a bar with "Before login" at the left and Suspend, Restart and
  Power off at the right.
- The person who logged in last (else the only person) is chosen when the screen opens, and PAM asks at once: the
  avatar, the name and the password field. Enter logs in. Esc (or the arrow at the left) cancels the login and shows
  the people and a user name field for another user.
- PAM behind greetd does the password and the fingerprint. A prompt is the field's placeholder; a secret prompt hides
  the text. An info message (for example "Place your finger on the fingerprint reader") shows while PAM waits.
- A wrong password shows "Wrong password. Try again." in one line under the field; the line keeps its height, so the
  screen does not move. Other PAM errors show PAM's own text there.
- After a good password the screen shows only the avatar and "Starting <session>" until greetd starts the session (the
  greeter ends then). It shows no login form again.
- You choose the session from `/usr/share/wayland-sessions` with the chips under the field. GNOME is first and is the
  default. The screen does not show sessions with `Hidden=true` or `NoDisplay=true`.
- The screen selects the session that the user started last when the user is chosen, else the first one, unless you
  chose one on this screen (todo-150). It keeps one `<user> <session>` line per user in
  `/var/lib/legionctl-greeter/last-sessions`; the last line is the person who logged in last. `greeter install` makes
  that folder for the user `greeter` (its home is `/`), and `greeter remove` keeps it. The greeter writes the line
  after greetd started the session; a failed write goes to the journal, and the session starts.
- X11 sessions (`/usr/share/xsessions`) are disabled chips; their tooltip gives the reason: the greeter runs in cage
  (Wayland) and starts no X server; log in to them through GDM.
- Power: Suspend, Restart and Power off ask once more ("Power off?" with the action and Cancel; Esc cancels), then
  call logind (`Suspend`, `Reboot`, `PowerOff`, with `interactive` false). polkit decides: the greeter's session is
  local and active, so logind's default rules allow it with no password while no other user is logged in.
- "Before login" opens the machine panel. Its choices for a user (Windows VM, Windows disk, NVIDIA GPU) are for the
  chosen user, else for the name typed; without one these buttons are disabled. The daemon's answer, or its error
  message in one sentence, shows in the bar:

| Row | Buttons | Daemon method (polkit action) |
| --- | --- | --- |
| Windows VM | Start, Stop | `GreeterAct(user, vm-start / vm-stop)` (`org.legionctl.greeter.act`) |
| Windows disk | Attach, Detach | `GreeterAct(user, disk-attach / disk-detach)` |
| NVIDIA GPU | Linux, VM | `GreeterAct(user, gpu-linux / gpu-none)` |
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
  start does. `Attach` starts the automounts again. The daemon refuses both while the Windows VM runs.
- After login, `legionctl windows disk attach|detach` (D-Bus `WindowsDisk`, polkit `org.legionctl.windows.vm`) runs
  the same function (note 22).
- **NVIDIA GPU**: `VM` is `gpu give nvidia none` for the typed user (`/dev/vfio/<group>` goes to that
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
- this version of legionctl's greeter is installed already (a second install must not overwrite the kept file).

An install over an older version of ours (for example r324: `cage -s` without `-m last`, no folder) makes only the
folder and greetd's configuration again. It keeps the kept file and changes no unit.

Install does these steps in this order:

1. It keeps `/etc/greetd/config.toml` as `/etc/greetd/config.toml.before`.
2. It writes `/etc/greetd/config.toml` (first line `# legionctl greeter`): VT 1,
   `systemd-cat -t legionctl-greeter cage -s -m last -- env -u DISPLAY FONTCONFIG_FILE=<fonts.conf> legionctl-greeter`
   as `greeter`.
3. It writes `/etc/systemd/system/greetd.service` and empties the NVIDIA module list (above).
4. `systemctl disable gdm.service`. GDM stays installed.
5. `systemctl enable greetd.service`. The `display-manager.service` alias is one link, so GDM must go first.

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
- The screen through gpui-kit's test support (`crates/legionctl/tests/greeter.rs`): a chosen person and session log
  in, and the next screen asks the same person at once with that session (Esc goes to another person, who gets the
  first session); after a good password no login control is on the screen until greetd answers; a wrong password
  gives the one line by the field and the next try logs in; a power action asks once more, Esc takes it back, and the
  confirmation calls a stand-in logind on a private bus; a machine choice for a user does nothing without a user and
  reaches a stand-in daemon with the typed name.
- Install and remove through the operator and the real daemon on the recorded machine: refused without cage (nothing
  changed); the old file kept byte for byte; GDM disabled before greetd is enabled; a second install refused; an
  install over r324's file makes our file and the folder again and changes no unit; remove puts the old file and GDM
  back.
- The screen as cage shows it: `EVIDENCE/validation/49-greeter-ui` (a headless cage and a screencopy client, before
  and after).
- `the_package_removal_stops_while_the_boot_takeover_or_a_greeter_without_gdm_is_installed`: the hook's `Exec` line
  on the recorded machine: it passes with nothing installed; it fails with the boot entry or with the greeter and no
  GDM, and names the command to run first; it passes again with GDM.
- `the_greeter_cannot_write_a_lasting_autostart_rule`: the greeter's autostart actions are refused and write no rule; a
  VM start without the user's rule is refused.
