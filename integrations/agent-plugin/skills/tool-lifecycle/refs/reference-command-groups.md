# Command groups

`legionctl --help` and `legionctl <group> --help` say what each command does. Class: R read, S session,
W workspace, M machine (polkit). "client" runs in the client only, with no operation.

| Group | Class | Runs in | Track with | Skill |
| --- | --- | --- | --- | --- |
| `status`, `load`, `doctor`, `capabilities`, `monitor`, `plan` | R | operator | `tui`, `doctor` | machine-tools |
| `validate` | R | operator | - | verify-and-ship |
| `gpu`, `vfio`, `knobs`, `fan`, `battery` | R, M | daemon | its `status`/`show` | machine-tools |
| `power`, `cleanup`, `tune`, `undo` | M | daemon (`undo` of `display.change`: as you) | `events`, `plan` | machine-tools |
| `rules` (with `sets`) | R, M | daemon; your own layer as you, then a reload | `rules list`, `plan` | machine-tools |
| `boot`, `kernel`, `greeter` | R, M | daemon; the next start | its `status`/`list` | machine-tools |
| `windows` | R, W, M | daemon, QEMU | `windows vm status` | machine-tools |
| `display` | R, W | operator, as you | `events`, `undo` | machine-tools |
| `virt` | R, W | docker, virsh, as you | `virt status` | machine-tools |
| `bench`, `record`, `replay` | R, W | operator, stress-ng, gpu-burn | `bench list`, `bench compare` | machine-tools |
| `resources` | R (the routed operation has its own class) | operator, then the routed operation | `events` | machine-tools |
| `run` | client, as you | - | - | machine-tools |
| `dossier`, `hooks`, `hook` (client) | R, S, W | operator | `dossier check`, `dossier nudges`, `hooks list` | dossier-practice |
| `workspace` | R, S, W | operator, git; `pack`: docker or QEMU, a job | `workspace status`, `workspace list` | workspace-practice |
| `session`, `jobs`, `tools`, `agents`, `gateway` | R, W | systemd scope, bwrap, docker or QEMU; `session run` is a job | `session list`, `jobs`, `trace` | sessions-and-agents |
| `mcp-proxy` | client | - | `tools calls` | sessions-and-agents |
| `packages`, `cache`, `images` | R, W | operator, network, docker buildx and compose; `images build` is a job | its `list`/`stats`/`status` | sessions-and-agents |
| `setup`, `reminders`, `skills`, `config` | R, W | operator, your config files (with backups) | its `check`/`list`/`explain` | - |
| `events`, `trace`, `tui`, `mcp` | R / client | operator | - | tool-lifecycle |
