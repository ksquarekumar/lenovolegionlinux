# 06 — Tuning

## What the sweep taught us

- The 2026-10-12 runs mixed in background load (WinBoat install, a manual `legionctl apply` mid-step), so their
  throughput numbers are unreliable. CPU-only, 55→70 W gave +14 % and 70→85 W about +0 % at 7 °C hotter, which
  suggests the knee near 70 W, but this needs a clean run.
- The sweep tuned RAPL PL1 while the firmware's own `ppt_pl1_spl` was 90 W. The real knobs are the firmware
  attributes, and `ppt_cpu_cl` is exactly the CPU limit under combined CPU+GPU load.

## Tuning engine in legionctl-system

`StartTune(spec)` runs inside the daemon, which already has every sensor:

- **Parameters**: `ppt_pl1_spl`, `ppt_pl2_sppt`, `ppt_cpu_cl`, optionally `cpu_temp`, `gpu_nv_ac_offset`; per
  firmware mode (`max-power`, `balanced`, `quiet`) so Fn+Q modes get their own tuned limits.
- **Scenarios**: CPU-only, GPU-only, cross-load (CPU + GPU), burst (20 s from cold).
- **Metrics per step**: workload score, package and GPU power, temperatures (avg/max), clocks, fan RPM (when
  validated), throttle reasons from MSR, run-queue latency p99 (eBPF).
- **Guards**: AC only, idle check (load and the workload classifier), reconciler paused for the tuned knobs,
  restore on any exit, abort on temperatures above the firmware limits.
- **Output**: results to `/var/lib/legionctl/tune/<time>-<attribute>.json`; the recommended value into the generated
  layer `/var/lib/legionctl/configsets/50-tuned.jsonc` (for AC), which is off until `legionctl rules enable tuned`.
- **Recommendation**: per scenario the knee (smallest limit within 2 % of the best score), constrained by no
  thermal throttle reasons in the steady-state half and GPU throughput within 2 % of its best for cross-load.

Built (2026-10-13, M3e): `legionctl tune <attribute> cpu|gpu|mixed <values...> [--seconds 60]
[--cooldown-s 30] [--force]`. The operator refuses a machine that is not idle (unless `--force`), resolves the
bench workloads (stress-ng, gpu-burn) to full paths, and calls `System1.Tune` (polkit action `org.legionctl.tune`).
The daemon refuses an attribute outside the list above, a value outside the firmware range (and an attribute
without a range), and battery. For each value it writes the attribute (with a read-back check), runs the workloads
as the caller (`setpriv` to the caller's uid and gid; nothing user-owned runs as root), samples telemetry each
second as root, and stops the step at once when the machine leaves AC or the CPU is hotter than the firmware's
`cpu_temp` (it kills the workload's process group). Events wait while a tune runs; at the end the original value
comes back, the policy applies again for the current power source (an Fn+Q choice stays), and a `Changed("Tune")`
signal goes to the event log. The report goes to `/var/lib/legionctl/tune/<time>-<attribute>.json`.
The recommended value goes into the generated layer `50-tuned.jsonc` (`{"when": {"power": "ac"}}`, beside the values
of earlier sweeps), off until `legionctl rules enable tuned` (todo-74, note 01). With `--per-kernel` (todo-84 part 3)
the value goes into the layer of the running kernel only: `50-tuned-<package>.jsonc`
(`{"when": {"power": "ac", "kernel": "<package>"}}`), off until `legionctl rules enable tuned-<package>`. Thus each
kernel can have its own limits (08-kernel.md, "Bench and tune per kernel"). The configset checks keep it out
while no layer gives the same limits for battery (the ac/battery rule); `doctor` names it. Not verified live: a sweep on the real machine
(needs the package installed, the polkit dialog and an idle machine) and the `setpriv` path (the e2e daemon runs as
the caller, so the workload runs directly).

## Results: sweep 2026-10-12 04:06 (background load 6–8, `--force`)

| Scenario | Finding | Decision |
| --- | --- | --- |
| CPU only | 70 W → 85 W: +12 % (31,757 → 35,511 ops/s), 80 → 89 °C avg. Above 85 W the package never exceeds ~89 W: firmware `ppt_pl1_spl` (90 W) is the real ceiling. | AC sustained RAPL limit raised 70 → 85 W in the profile. |
| CPU + GPU (gpu-burn, GPU at 174 W) | CPU package stays at 54–60 W whatever PL1 says; 96–97 °C with ~30,000 thermal throttle events per 3 min. GPU throughput unchanged (~21,600 Gflop/s). Best CPU score at 70 W; 55 W costs 2.5 % but runs 6 °C cooler with a third of the throttling. | Heat-limited, not power-limited. Next: test firmware `ppt_cpu_cl` (75 W) at ~60 W directly. |
| Burst (20 s from cool, PL1 70 W) | PL2 90 W throttles (1,576 events, 94 °C); 110 W and 125 W do not. | Keep PL2 at 125 W. |

The 33,251 package throttle events seen since boot come from combined CPU+GPU load (thermal), not CPU-only work.
