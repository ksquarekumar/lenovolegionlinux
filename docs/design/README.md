# legionctl design notes

The design of legionctl: one operator, one API, a machine engine and an agent engine. Start with
[15-layout](15-layout.md) for names, processes and the command tree, then [00-overview](00-overview.md) for the
goals and the architecture. Each note has its decisions and open questions inline.

State on 2026-10-16, main `e8ccd09`. Plan rows (`todo-NN`) and `DESIGN-NN`, `MEMORY-NN` are in the dossier
`legionctl` (`~/agentWorkspaces/legionctl`, read with `legionctl dossier read legionctl/plan.md`). The planned graph
of the open work is in [planning/](../../planning) (`legionctl code plan status`).

| State | Meaning |
| --- | --- |
| built | on main; tests and CI pass |
| building | built on a topic branch, not merged |
| planned (v1) | approved, in the v1 cut (MEMORY-16) |
| post-v1 | approved for after v1 |
| done | a plan that ran to its end; the note is history |
| waits for the user | built; a live step only the user can do |

## Notes

| Note | Covers | State |
| --- | --- | --- |
| [00-overview](00-overview.md) | goals, constraints measured on this machine, architecture | built |
| [01-core](01-core.md) | event bus, configsets, reconciler, ownership ledger, safety | built (configsets: todo-74; five deviations wait for the user) |
| [02-platform](02-platform.md) | each control surface: firmware attributes, RAPL, cpufreq, PPD, sched_ext, NVIDIA, display, fans | built (display stack todo-81); fan curve writes wait for the user (todo-17) |
| [03-session](03-session.md) | the operator in the graphical session, the OSD extension, the GPUI app | built |
| [04-ebpf](04-ebpf.md) | eBPF sensors: run-queue latency, exec, workload, power share, throttle, block I/O, thermal, session files and memory | built (todo-22, 23, 96); HID-BPF not built |
| [05-windows](05-windows.md) | dual boot, restart into Windows, the installed Windows under KVM, GPU give | built (todo-28, 29); first live moves wait for the user. Windows repair and host actions from Windows: planned (v1, todo-151) |
| [06-tuning](06-tuning.md) | the firmware-limit sweep in the daemon | built (todo-21); the live sweep waits for the user |
| [07-plan](07-plan.md) | the phased move from the Python tooling | done (switch 2026-10-13) |
| [08-kernel](08-kernel.md) | modules and DKMS, `legionctl_wmi`, boot takeover, kernels | built: `legionctl_wmi` part 1 (opt-in, todo-79, todo-143), boot entry (todo-80; not installed live), kernels (todo-84). Boot tool checks: building (todo-149) |
| [09-ux](09-ux.md) | the first UI structure: disclosure levels, pages | built, then superseded in part by DESIGN-09 (below) |
| [10-gap](10-gap.md) | parity with Lenovo Legion Toolkit and Vantage | parts 1 and 3 built (todo-82); part 2 waits for todo-17 |
| [11-disks](11-disks.md) | disk health, NTFS mounts and repair | built in part: `windows status` (dirty NTFS), `windows disk attach/detach`; repair through Windows: planned (v1, todo-151) |
| [12-operator](12-operator.md) | the operator and the agent harness: one core, one API for every surface | built |
| [13-daemon](13-daemon.md) | processes, one-writer SQLite, event log, outbox, failure isolation | built |
| [14-sessions](14-sessions.md) | sessions: sandboxes, git gate, MCP proxy and cache, ACP, pause and resume, layered config | built |
| [15-layout](15-layout.md) | names, processes, operation registry, crates, paths, command tree, test policy | built; the authority for names |
| [16-observability](16-observability.md) | see, check, change, record and replay tools | built (M3a-M3g) |
| [17-virtualization](17-virtualization.md) | VMs, containers, GPU per run, VFIO | built (todo-69, 75); live GPU use waits for the user |
| [18-workspaces](18-workspaces.md) | dossiers, workspaces, topic workspaces, git operations, the no-dossier gate | built (todo-1, 86, 103 P9) |
| [19-gateway](19-gateway.md) | the model gateway: addressing, budgets, translation, semantic cache | built (G1, G2) |
| [20-images](20-images.md) | images, compose files, VM disks, `workspace pack` | built (I1) |
| [21-greeter](21-greeter.md) | greetd and legionctl's login screen in place of GDM | built and installed (todo-83); last desktop per user: planned (v1, todo-150) |
| [22-resources](22-resources.md) | one resource list and its actions; saved configset sets; hidden boot entries | built (todo-85) |
| [23-agent-network](23-agent-network.md) | code graphs, the agent network, the thread UI, project checks, rlcd | see the parts below |
| [issues](issues.md) | issues found on this machine, cleanups | open list |

### Parts of 23-agent-network

| Part | State |
| --- | --- |
| A: two code graphs (P1-P4) | built (todo-90) |
| B and B+: threads, members, outbox, presence, archetypes, teams (P5-P6c) | built (todo-91, 97, 98) |
| C: the thread UI in the TUI and the app (P7-P8) | built (todo-92) |
| E: dossier home (P9), project checks (P10) | built (todo-103, 100); `.legionctl/checks.jsonc` of this repository: building (P10b, waits for todo-147) |
| E: change watcher, test runners, graph schema, observer recovery (P11-P14) | planned (v1, todo-101 to 104) |
| F: rlcd, the System One tier of a wake | core built (todo-138); the `api` step and hot paths: planned (v1, todo-139) |
| G: projects, runnables and runs per project | planned (v1, todo-140); the text is still to write |

## Designs in the dossier

These designs are in the dossier, not in this folder. Each has its plan rows.

| Design | Covers | State |
| --- | --- | --- |
| DESIGN-09 | the app: spaces Machine, Work, Agents and Activity; tabs; every screen a view that agents can remake; the same views in the TUI and the CLI | G1 building (topic `gen-ui`); G2-G6 planned (v1, todo-145, 146, 141) |
| DESIGN-10 | the temporal, ref-bound graph: valid time, recorded time, ref range, content anchor; the agent query layer | T1 building (topic `gen-ui`); T2-T5 planned (v1, todo-152) |
| DESIGN-11 | agent topology: bound main and observers, the product-development track, one SDK for all surfaces | A1, A2, A4 planned (v1, todo-153); A3, A5 post-v1 |
| DESIGN-04 | the design map of 2026-10-14 | superseded by this index |

## Planned (v1)

The v1 cut (MEMORY-16, user 2026-10-16) holds every open plan row except the post-v1 list. The larger items:

| Item | Plan row | Planned graph |
| --- | --- | --- |
| Generative UI G2-G6 | todo-145, 146, 141 | `planning/features/generative-ui.jsonc` |
| Temporal graph, watcher, rules, agent query layer | todo-152, 101-104 | `temporal-graph.jsonc` |
| Agent topology A1, A2, A4 | todo-153 | `agent-topology.jsonc` |
| Environments: network isolation, declared connections, snapshots | todo-153 | `environments.jsonc` |
| Projects: nodes, dependencies, runnables, images, runs | todo-140 | `projects.jsonc` |
| Accounts per member, usage, switch on limit | todo-136 | `accounts.jsonc` |
| rlcd `api` step and hot paths | todo-139 | `rlcd.jsonc` |
| Windows care: backup, WinRE, repair, restore points, host actions | todo-151 | `windows-care.jsonc` |
| Build output, language tools, tooling manifests, tuning for agent work | todo-111, 113, 114, 137 | `tooling-and-tuning.jsonc` |
| Compaction defers to the dossier; immutable entries; dossier health | todo-156 | `compaction-and-dossier-health.jsonc` |
| Codebase quality | todo-154 | `quality.jsonc` |
| One coherent CLI, TUI and app (regroup), then the logo | todo-78, 89 | last |

Post-v1: the agent pool (A3) and bound skill agents (A5); the topic diary as a formal artifact, a public SDK, ports
across hosts, capability matching; a scheduled Windows health check and more repair steps.

Evidence for each machine claim was collected on this machine (CachyOS, kernel 7.2.9, GNOME 51, BIOS KWCN54WW,
EC 1.51) from 2026-10-12.
