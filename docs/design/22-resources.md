# 22 — Resources: one list and one action for the machine after start

Decided by the user 2026-10-14 (todo-85). Designed and built 2026-10-15 ("Built" below says what exists).

The user wants to attach, detach, start, pause, resume and switch to the resources, configurations and mounts of
the machine after the start, from two places that use one model:

1. Before login: hidden boot menu entries choose a kernel and a saved set for this start.
2. After login: one page in the app (`legionctl-app resources`) and one view in `legionctl tui`.

## The model: two operations

| Operation | Class | What it does |
| --- | --- | --- |
| `resources.list` | read | Each resource: its group, its id, its name, its state, what uses it, and its actions. Each action names the operation and the input that it runs, and the class of that operation. |
| `resources.act <id> <action> [--dry-run]` | read | Finds the resource and the action in the same list, then the operator runs the named operation with that input as a call of its own. |

`resources.act` has no implementation of an action. It only finds the route. The operator then runs the routed
operation through the same path as a direct call: the same input check, the same session gate (an agent's
`workspace`-class call asks the user), the same daemon call and polkit dialog, and the same event (its kind is the
routed operation, for example `knobs.release`). Thus `resources.act` is class `read`: the routed operation has its
own class. The answer has the routed operation, its input and its output, and the CLI shows the output as that
operation shows it.

`--dry-run`: when the routed operation has a dry run (`dry_run` in its input), it runs with `dry_run` and shows its
plan. Else nothing runs, and the answer shows the route only. A dry run changes nothing in both cases.

A refusal comes before a change: an unknown id or action names the ids or actions that exist. The routed operation
makes its own plan first, so its refusal also comes before the polkit dialog.

### Groups and routes

| Group | Resource (id) | State | What uses it | Actions -> operation |
| --- | --- | --- | --- | --- |
| vm | `vm:windows`, the installed Windows in a VM | running, paused, stopped | - | start -> `windows.vm.start`; pause -> `windows.vm.pause`; resume -> `windows.vm.resume`; stop -> `windows.vm.stop` |
| vm | `virt:<name>`, each container and VM of `virt status` (WinBoat is named) | the provider's state | the app it belongs to | start, pause, resume, stop -> `virt.act` |
| gpu | `gpu:nvidia` | linux, windows, none | the processes that have `/dev/nvidia*` open | give-linux, give-windows, give-none -> `gpu.give` |
| gpu | `desktop-gpu` | nvidia, intel, auto | - | nvidia, intel, auto -> `gpu.primary` (next login) |
| disk | `mount:<mount point>`, each mounted block device and each `/etc/fstab` entry | mounted, not mounted | the processes whose working folder, root or an open file is on it | the Windows disk's partitions: attach, detach -> `windows.disk` |
| configset | `layer:<id>` | on, off; used now or not | - | enable -> `rules.enable`; disable -> `rules.disable` |
| configset | `knob:<id>`, each held knob | held at a value | - | release -> `knobs.release` |
| configset | `set:<name>`, each saved set | saved; with boot entries or not | - | switch -> `rules.sets.switch`; remove -> `rules.sets.remove` |
| session | `session:<id>` | running, paused, ended | its workspace | pause -> `session.pause`; resume -> `session.unpause`; stop -> `session.stop`; start (ended) -> `session.resume` |
| session | `workspace:<path>` | its branch | the sessions that work in it | start-<agent> -> `session.start` (one per agent of `session.agents`) |

Only the actions that apply to the state are in the list (a running VM has no start). The list does not show an
action that needs a value which a person must type (a knob's new value, a new set's name): use the command for it
(`knobs hold`, `rules sets save`).

### Operations that did not exist

The routes above need these operations. Each is a normal operation, also in the CLI and MCP:

| Operation | Why | How |
| --- | --- | --- |
| `windows.vm.pause`, `windows.vm.resume` | The Windows VM had start and stop only. | QMP `stop` and `cont` on the VM's QMP socket, as you (class `workspace`). |
| `windows.disk attach\|detach [--dry-run]` | After login, only the greeter could attach or detach the Windows disk. | The daemon's `WindowsDisk(attach)` (polkit `org.legionctl.windows.vm`) runs the same function as the greeter's `disk-attach` and `disk-detach`: the disk's automounts and mounts start or stop. The daemon refuses it while the Windows VM runs. |
| `rules.sets.save <name> [--boot]`, `rules.sets.switch <name> [--dry-run]`, `rules.sets.remove <name>` | Saved sets (below). | The daemon's `Sets(action, name, boot)` (polkit `org.legionctl.rules`; with boot entries also `org.legionctl.boot`). |

Mount and unmount of other volumes (not on the Windows disk) are not actions: the root file system, `/home` and the
ESP must stay. Such a mount shows its state and its users only.

## Saved sets

A saved set is a named configset selection: the on and off state of the layers and the knob holds. The daemon keeps
both in `system.db` (tables `layers` and `holds`, 01-core.md). It keeps the saved sets beside them, in the table
`sets` (name, the selection as JSON). There is no second source: a set holds a copy of the two tables, and a switch
writes the two tables back.

| Command | What it does |
| --- | --- |
| `legionctl rules sets save <name> [--boot]` | Keeps the layer states and the holds of now as `<name>`. With `--boot`, each kernel's image gets a boot entry for the set (below). |
| `legionctl rules sets switch <name> [--dry-run]` | One change: the layer states and the holds become those of `<name>`. Holds that the set does not have are released; the holds of the set are written; then the layers apply. The daemon sends one `Changed` signal (`SwitchSet`), so the event log has one event (`machine.switch-set`). |
| `legionctl rules sets switch previous` | Undo: the selection before the last switch. The daemon keeps it as the set `previous` at each switch, so a second undo goes forward again. |
| `legionctl rules sets remove <name>` | Removes the set (and its boot entries). |

`previous` and `before-boot` are the daemon's names; `save` refuses them. `legionctl undo <event>` of a set switch is
refused with the pointer to `rules sets switch previous`: undo holds the old values of the writes, and a set switch
is a change of the selection, not of single values.

## Boot entries: a kernel and a set for this start

With the boot takeover (08-kernel.md: systemd-boot as shim's second stage, one signed UKI for each kernel), each set
that was saved with `--boot` gets a boot entry for each kernel. The menu stays hidden: `loader.conf` has `timeout 0`,
so systemd-boot starts the default entry at once, and shows the menu only when you hold a key at start.

How: each kernel's UKI is a multi-profile UKI (systemd-stub(7), "Multi-profile UKIs", systemd 257 and later).
Profile 0 has no command line of its own, so the kernel's own line applies: it is the default entry. Each boot set
adds one profile with the kernel's line and `legionctl.set=<name>`, and the title `<kernel> with <name>`.
systemd-boot shows each profile as an entry. Why not a type #1 entry with `options`: with Secure Boot on, systemd-stub
ignores a command line from the boot loader when the UKI has its own, so the set must be in the signed image. The
profiles are only a few bytes, so there is one image for each kernel as before.

The build is the one build of the boot takeover (`boot install`, `boot rebuild`, the pacman hook,
`kernel cmdline set`): `ukify build` makes one profile file for each boot set (`--profile`, `--cmdline`), then the
image with `--join-profile` for each, signed as before; `sbverify` checks it, and `mv` puts it in place. The profile
files are removed after it. `rules sets save --boot` and `rules sets remove` make the images again when the set has
boot entries.

Without the boot takeover, `rules sets save <name> --boot` refuses with nothing changed: "install the boot takeover
first (legionctl boot install)". A set without `--boot` needs no boot takeover.

### The daemon at start

Before its first apply, the daemon reads `/proc/cmdline`:

- `legionctl.set=<name>`: it keeps the selection of now as `before-boot` (only when there is none, so two starts in a
  row through the menu do not lose it), then switches to `<name>`.
- No `legionctl.set=`, and `before-boot` exists: it switches back to `before-boot` and removes it.

So the set applies for this start only. A set that does not exist is logged, and nothing changes. The start needs no
polkit: the set was saved with polkit, and the boot menu needs a person at the keyboard.

## Surfaces

- CLI: `legionctl resources list`, `legionctl resources act <id> <action> [--dry-run]`.
- MCP: `resources_list`, `resources_act`.
- App: the page **Resources** (`legionctl-app resources`). Each group is a section; each resource is a row with its
  name, state and users, and one button for each action. A click runs `resources.act` with `--dry-run` first: the
  page shows the plan (or the route) with Apply and Discard, as the power policy does.
- TUI: `legionctl tui`, the key `r` shows the resources view. `Up` and `Down` select a row. The keys `1` to `9`
  choose an action of the row and show its dry run. `Enter` applies it. `Esc` discards it.

## Tests (e2e)

On the recorded machine fixture, with the real operator and daemon and the stand-ins (polkit, systemctl, the ESP,
efibootmgr, ukify, sbverify):

- `resources_show_each_group_and_each_action_runs_the_existing_operation` (`tests/machine.rs`): the list shows the
  mounted Windows partition with the process that uses it, a running session, a held knob and the layers; the dry run
  of a detach changes nothing; each act gives the effect and the event of the routed operation (the knob is released
  through the daemon with polkit, the session is paused, the disk's automounts stop through the daemon with polkit);
  an unknown action is refused with nothing changed.
- `a_saved_set_switches_layers_and_holds_as_one_change_and_switch_previous_undoes_it` (`tests/machine.rs`).
- `boot_entries_for_each_kernel_and_set_and_the_set_of_the_start_applies_at_daemon_start` (`tests/machine.rs`):
  refused without the boot takeover; then each image has a profile for each boot set with `legionctl.set=`, the
  menu stays hidden; a daemon start with `legionctl.set=` on the stand-in `/proc/cmdline` applies that set, and the
  next start without it puts the selection before back.
- `the_resources_page_lists_the_resources_and_an_action_click_calls_its_operation` (`tests/app.rs`, gpui-kit test
  support).
- `the_windows_vm_pauses_and_resumes_through_qmp_and_the_list_follows` (`tests/machine.rs`, the stand-in QMP of the
  GPU tests): pause without a VM is refused and nothing reaches QMP; pause sends `stop`, the list says paused,
  resume sends `cont`.
- `the_tui_resources_view_lists_the_resources_and_a_key_runs_the_route` (`tests/app.rs`): `legionctl tui` in a pty;
  `r`, Down, `1` show the route of a layer's disable, Enter runs `rules.disable` with the TUI as its actor.

The operator of the app and TUI tests gets `DOCKER_HOST` to a socket that does not exist: those tests click
actions, and must not reach this machine's containers.

## Built (2026-10-15)

- `legionctl-api`: `resources.list`, `resources.act`, `windows.vm.pause|resume`, `windows.disk`,
  `rules.sets.save|switch|remove`; the schema in `docs/api/schema.json`.
- Operator: `crates/legionctl-operator/src/resources.rs` makes the list and finds the route; `server.rs` runs the
  routed operation through the same `run` as a direct call. `undo` of a `machine.switch-set` event refuses with the
  pointer to `rules sets switch previous`.
- Daemon: `WindowsDisk(attach)` (the greeter's `disk-attach` and `disk-detach` use the same function), `Sets(action,
  name, boot)`, and the set of the start before the first apply. `system.db` has the table `sets`.
- Machine crate: `mounts.rs` (mounts and the processes that use them, from `/proc/<pid>/cwd` and `fd`), `sets.rs`
  (the selection, the names, the dry-run lines), the profiles in `boot.rs`'s one image build.
- App: the page **Resources**. TUI: the key `r`. The CLI runs a session that `resources act` starts in the
  terminal, as `session start` does.

What uses a mount is found only for the processes whose links the operator may read: your own processes. A process
of another user (root) is not shown.

## Open

- The ukify flags for multi-profile images (`--profile`, `--join-profile`) and the entry ids of profiles in
  systemd-boot (the default entry must stay `legionctl-<kernel>.efi`) are from the systemd 257 documentation. The
  stand-in `ukify` of the tests records the flags only. The first live `boot rebuild` with a boot set is the user's;
  check the menu (hold a key at start) and `bootctl list`.
