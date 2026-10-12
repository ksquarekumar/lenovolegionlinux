# 06 — Tuning

## What the sweep taught us

- The 2026-10-12 runs mixed in background load (WinBoat install, a manual `legionctl apply` mid-step), so their
  throughput numbers are unreliable. CPU-only, 55→70 W gave +14 % and 70→85 W about +0 % at 7 °C hotter, which
  suggests the knee near 70 W, but this needs a clean run.
- The sweep tuned RAPL PL1 while the firmware's own `ppt_pl1_spl` was 90 W. The real knobs are the firmware
  attributes, and `ppt_cpu_cl` is exactly the CPU limit under combined CPU+GPU load.

## Tuning engine in legiond

`StartTune(spec)` runs inside the daemon, which already has every sensor:

- **Parameters**: `ppt_pl1_spl`, `ppt_pl2_sppt`, `ppt_cpu_cl`, optionally `cpu_temp`, `gpu_nv_ac_offset`; per
  firmware mode (`max-power`, `balanced`, `quiet`) so Fn+Q modes get their own tuned limits.
- **Scenarios**: CPU-only, GPU-only, cross-load (CPU + GPU), burst (20 s from cold).
- **Metrics per step**: workload score, package and GPU power, temperatures (avg/max), clocks, fan RPM (when
  validated), throttle reasons from MSR, run-queue latency p99 (eBPF).
- **Guards**: AC only, idle check (load and the workload classifier), reconciler paused for the tuned knobs,
  restore on any exit, abort on temperatures above the firmware limits.
- **Output**: results to `/var/lib/legion/tune/<timestamp>.jsonl`; a recommended layer to
  `/var/lib/legion/configsets/tuned.toml` that is inactive until the user enables it.
- **Recommendation**: per scenario the knee (smallest limit within 2 % of the best score), constrained by no
  thermal throttle reasons in the steady-state half and GPU throughput within 2 % of its best for cross-load.

`scripts/rapl-sweep.sh` stays until the engine exists; next revision should sweep `ppt_*` instead of RAPL
(after the writability question in 02-platform is answered).

## Results: sweep 2026-10-12 04:06 (background load 6–8, `--force`)

| Scenario | Finding | Decision |
| --- | --- | --- |
| CPU only | 70 W → 85 W: +12 % (31,757 → 35,511 ops/s), 80 → 89 °C avg. Above 85 W the package never exceeds ~89 W: firmware `ppt_pl1_spl` (90 W) is the real ceiling. | AC sustained RAPL limit raised 70 → 85 W in the profile. |
| CPU + GPU (gpu-burn, GPU at 174 W) | CPU package stays at 54–60 W whatever PL1 says; 96–97 °C with ~30,000 thermal throttle events per 3 min. GPU throughput unchanged (~21,600 Gflop/s). Best CPU score at 70 W; 55 W costs 2.5 % but runs 6 °C cooler with a third of the throttling. | Heat-limited, not power-limited. Next: test firmware `ppt_cpu_cl` (75 W) at ~60 W directly. |
| Burst (20 s from cool, PL1 70 W) | PL2 90 W throttles (1,576 events, 94 °C); 110 W and 125 W do not. | Keep PL2 at 125 W. |

The 33,251 package throttle events seen since boot come from combined CPU+GPU load (thermal), not CPU-only work.
