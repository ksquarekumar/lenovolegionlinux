# Command groups

`legionctl --help` and `legionctl <group> --help` say what each command does. Class: R read, S session,
W workspace, M machine (polkit). "client" runs in the client only, with no operation.

| Group | Class | Runs in | Track with | Skill |
| --- | --- | --- | --- | --- |
| `status`, `load`, `sensors`, `doctor`, `capabilities`, `monitor`, `plan` | R | operator | the app, `doctor` | machine-tools |
| `validate` | R | operator | - | verify-and-ship |
| `gpu`, `vfio`, `knobs`, `fan`, `battery` | R, M | daemon | its `status`/`show` | machine-tools |
| `power`, `cleanup`, `tune` | M | daemon | `events`, `plan` | machine-tools |
| `undo` | W | daemon (`undo` of `display.change`: as you) | `events`, `plan` | machine-tools |
| `rules` (with `sets`) | R, W (`edit`, `reset`), M | daemon; your own layer as you, then a reload | `rules list`, `plan` | machine-tools |
| `boot`, `kernel`, `greeter` | R, W (`kernel build`), M | daemon; the next start | its `status`/`list` | machine-tools |
| `windows` | R, W, M | daemon, QEMU | `windows vm status` | machine-tools |
| `display` | R, W | operator, as you | `events`, `undo` | machine-tools |
| `virt` | R, W | docker, virsh, as you | `virt status` | machine-tools |
| `bench`, `record`, `replay` | R, W | operator, stress-ng, gpu-burn | `bench list`, `bench compare` | machine-tools |
| `resources` | R (the routed operation has its own class) | operator, then the routed operation | `events` | machine-tools |
| `run` | client, as you | - | - | machine-tools |
| `dossier`, `hooks`, `hook` (client) | R, S, W | operator | `dossier check`, `dossier nudges`, `hooks list` | dossier-practice |
| `code graph` (`refresh`, `refs`, `query`, `impact`, `changes`, `annotate`, `annotations`, `prune`, `hook`) | R, S, W (`hook`) | operator, ctx | `code graph refs`, `code graph changes` | workspace-practice |
| `code tests` (`run`, `list`, `show`, `review`, `mutant record`, `mutant run`) | R, S | operator, the repository's `test` checks, cargo-mutants; `run` and `mutant run` are jobs | `code tests review`, `jobs` | verify-and-ship |
| `code checks` (`detect`, `list`, `run`, `show`) | R, S (`run`) | operator, `<repo>/.legionctl/checks.jsonc`, the language files; `run` is a job | `code checks list`, `code checks show` | verify-and-ship |
| `code plan` (`check`, `status`) | R | operator, ctx, tokei | `code plan status` | workspace-practice |
| `workspace` | R, S, W | operator, git; `pack`: docker or QEMU, a job | `workspace status`, `workspace list` | workspace-practice |
| `thread` (`start`, `list`, `show`, `join`, `leave`, `send`, `inbox`, `ack`, `declare`, `pace`, `rules`, `peers`, `observe`, `import`, `branch`, `compensate`) | R, S, W (`start`, `join`, `leave`, `import`, `branch`, `compensate`) | operator; delivery through the hooks; team members wake as operator sessions | `thread show`, `thread peers`, `trace` | sessions-and-agents |
| `views` (`list`, `check`, `save`, `render`, `place`) | R, S (`place`), W (`save`) | operator; the source operation as the caller | `views render`, the app | tool-lifecycle |
| `session`, `jobs`, `tools`, `agents`, `gateway` | R, W | systemd scope, bwrap, docker or QEMU; `session run` is a job | `session list`, `jobs`, `trace` | sessions-and-agents |
| `mcp-proxy` | client | - | `tools calls` | sessions-and-agents |
| `packages`, `cache`, `images` | R, W | operator, network, docker buildx and compose; `images build` is a job | its `list`/`stats`/`status` | sessions-and-agents |
| `setup`, `reminders`, `skills`, `config` | R, W | operator, your config files (with backups); `setup turn-on` starts a service of legionctl or clauth's daemon (polkit for `legionctl-system`) | its `check`/`list`/`explain` | - |
| `events`, `trace`, `mcp` | R / client | operator | - | tool-lifecycle |
