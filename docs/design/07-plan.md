# 07 — Migration plan

Each phase ships something usable. The Python tooling runs until `legionctl-system` passes the parity gate; one
change then replaces it (15-layout). No phase leaves two implementations installed.

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

## Phase 1 — legionctl-system with parity, then the switch (weeks)

- `legionctl-machine`: sysroot (recorded-tree testing), sensors (udev, sysfs notify, logind, evdev), actuators
  (platform_profile, firmware attributes, cpufreq, sysctl, PPD, scx_loader, NVIDIA units). No RAPL writes.
- Profile JSON → configset TOML converter with the same validation rules as `legionctl validate`.
- `legionctl-system` with `status`, `doctor`, `plan`, `apply`, `validate`, `follow` semantics through the ledger.
- **Parity gate**: on this machine, for AC, battery, Fn+Q override, resume and boot, `legionctl-system --dry-run`
  plans the same writes as the Python tooling (except RAPL), compared in CI from recorded fixtures.
- **Switch** (one change): install `legionctl-system`; delete the Python `legionctl`, `legion-osd`,
  `legion-refresh-toggle`, the reassert timer, the follow service, the udev `RUN+=` rules, the sleep hook and the
  hwdb remap; rename the Rust client binary to `legionctl`.

## Phase 2 — session

- Operator session module: Mutter actions (Fn+R, refresh per layer), OSD requests.
- GNOME Shell extension `legionctl-osd@legionctl`: animated OSD (03-session).

## Phase 3 — observability and tuning

- MSR throttle-reason sensor; validated fan source; `legionctl power monitor`.
- eBPF sensors: workload classifier, run-queue latency, power attribution, session activity.
- Tuning engine in the daemon sweeps `ppt_*`; delete `scripts/rapl-sweep.sh` in the same change.

## Phase 4 — GPUI app

`legionctl-app`: live view, configset editor with plan preview, tuning wizard, Windows panel, agent sessions.

## Phase 5 — Windows

Hazard detection (hivex read-only), `RebootToWindows`, VFIO VM of the installed Windows with guard rails.
No Windows-side agent: Lenovo Vantage covers Windows (decided 2026-10-12).

## Testing

- End-to-end at a boundary: `legionctl-system --dry-run` against recorded sysroots of this machine
  (`tests/fixtures/sysroot-82wq-*`), with uevent replay for Fn+Q bursts, AC changes and resume; the CLI and MCP
  server against a temporary operator database.
- Narrow tests only for failures that are not obvious (ledger decisions on firmware-initiated changes, phantom I/O
  pressure). Tests that restate the code are deleted.
- On-machine `make selftest`: the daemon in dry-run against the real system, then `doctor`.
- CI: `cargo fmt --check`, `clippy -D warnings`, `cargo test`, `cargo deny`, and the pre-commit hooks.
