# Measurement Workflow

Use this before importing DKMS features or adding hotkey write actions. The
goal is to capture what this 82WQ machine actually emits for Legion hotkeys,
WMI events, display modes, fan telemetry, and power state.

## Hotkey And Event Capture

Run:

```bash
sudo -E ./scripts/capture-capabilities.sh
```

The script writes a directory under `/tmp/legion-capability-capture-*`. It
records:

- kernel journal messages
- udev kernel/udev property events
- `acpi_listen`, if installed
- `libinput debug-events`, if installed
- before/after sysfs, WMI, display, NVIDIA, fan, and thermal snapshots
- timestamp markers for `Fn+Q`, `Fn+R`, `Fn+Space`, and `Fn+Esc`

Use `sudo -E` so display-related commands have the best chance of seeing the
active desktop environment.

For a non-interactive timed capture:

```bash
sudo -E ./scripts/capture-capabilities.sh --duration 30
```

## What The Capture Decides

| Evidence | Decision |
| --- | --- |
| `Fn+Q` produces WMI thermal-mode events | Add event sync so `legionctl` reacts to hardware mode changes instead of only reasserting policy. |
| `Fn+R` produces an input/ACPI/WMI event | Add a userspace refresh-rate action that enumerates real panel modes. |
| `Fn+Space` produces input/ACPI/WMI or lighting-controller events | Decide whether keyboard lighting belongs in WMI, HID/RGB, or should remain firmware-only. |
| Read-only DKMS hwmon reports sane fan RPM | Promote DKMS hwmon import as the first kernel feature. |
| DKMS fan telemetry conflicts with `platform_profile` or NVIDIA Dynamic Boost | Do not import it as-is; split or patch the driver further. |

## Current Import Gate

Do not add fan write support until read-only fan RPM is trustworthy. Do not add
Fn+R refresh switching until the capture shows both the hotkey event and the
actual panel modes available on this machine.
