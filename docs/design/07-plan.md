# 07 — Migration plan

Each phase ships something usable and keeps the previous tooling working until parity is proven.

## Phase 0 — fixes in the current Python tooling (days)

Cheap, independent, and they de-risk the Rust work by answering open questions.

1. Resume hook: `/usr/lib/systemd/system-sleep/legion` runs `legionctl apply auto` after resume.
2. `reassert` also restores RAPL (or switches to firmware `ppt_*`, see 4).
3. Primary GPU: udev rule tagging the NVIDIA card `mutter-device-preferred-primary` while the external monitor
   is the main display; re-login to test the choppy-animation fix. Measure battery impact.
4. Answer the firmware-attribute writability question (`ppt_*` in `max-power` vs `custom`), then add a
   `firmware_limits` block to the profile and make `apply` write it.
5. `doctor`: throttle reasons (MSR), bogus thinkpad fan source, RTC/Fast Startup hazards (read-only hive check),
   conservation mode, camera power.
6. Package installs through `makepkg -si` instead of `make install` (tracked files, clean removal).
7. Fan RPM from firmware WMI via `acpi_call` (done; validate idle vs `gpu-burn`).
8. Kernel module inventory in `doctor` (08-kernel): required/optional/unwanted, DKMS built per kernel, autoload drift.
9. GPU mode detection in `status`; `legionctl run --gpu nvidia|intel --cores p|e -- cmd`; primary-GPU toggle.

## Phase 1 — legiond core with parity (weeks)

- Workspace, `legion-core` (configsets, merge, validate, plan), `legion-sys` (fake-root testing),
  sensors: udev, sysfs notify, logind, evdev; actuators: platform_profile, firmware attributes, RAPL, cpufreq,
  sysctl, PPD, scx_loader, NVIDIA units.
- Profile JSON → configset TOML converter; same validation rules as `legionctl validate`.
- Rust `legionctl` with `status`, `doctor`, `plan`, `apply`, `validate`, `follow` semantics via the ledger.
- **Parity gate**: on this machine, for AC, battery, Fn+Q override, resume and boot, the Rust daemon in
  `--dry-run` plans exactly the writes the Python tooling performs (diffed in CI from recorded fixtures).
- Then: switch over, remove the reassert timer, follow service and udev `RUN+=` rules.

## Phase 2 — session

- `legion-agent` with Mutter actions (Fn+R, refresh per layer) and OSD backend selection.
- GNOME Shell extension `legion-osd@legion` for native OSD; notifications as fallback.
- Retire `legion-osd` and `legion-refresh-toggle`.

## Phase 3 — observability and tuning

- MSR throttle-reason sensor; fan source validated (02-platform options); `legionctl monitor`.
- eBPF plugins: workload classifier, run-queue latency, power attribution.
- Tuning engine in the daemon; retire `scripts/rapl-sweep.sh`.

## Phase 4 — UI: web (wasm) + desktop (Dioxus), GPUI optional

`legion-ui` (Dioxus) built for wasm (served by legion-agent) and desktop; live view, configset editor with plan
preview, tuning wizard, Windows panel. GPUI `legion-center` only if a native-first variant is still wanted.

## Phase 5 — Windows

Hazard detection (hivex read-only), `RebootToWindows`, VFIO VM of the installed Windows with guard rails.
No Windows-side agent: Lenovo Vantage covers Windows (decided 2026-10-12).

## Testing strategy

- Unit tests in `legion-core` (merge, validation, plan, ledger decisions) — the bulk.
- Integration tests against a fake `SysRoot` with recorded sysfs trees from this machine
  (`tests/fixtures/sysroot-82wq-2026-10-12/`), including uevent replay for Fn+Q bursts and AC changes.
- On-machine `make selftest`: daemon in dry-run against the real system, then `doctor`.
- CI: `cargo fmt --check`, `clippy -D warnings`, `cargo test`, `cargo deny`, plus the existing pre-commit hooks.
