---
name: machine-tools
description: How to read and change this Lenovo Legion Pro 7 16IRX8H with legionctl (power, knobs and configset rules, fan, battery, display, GPU, kernel, boot, greeter, Windows, virtualization, bench, tune, record, replay, undo) and the user's safety rules for agents. Use before each legionctl machine command and when the user asks about power, heat, fans, GPU, display, kernels or boot.
user-invocable: true
---

# Machine tools

## Safety rules for agents (from the user)

- "do not guess ACPI/WMI write calls on the real firmware"
- "never run `windows reboot`, tune sweeps or polkit writes live"
- "do not pause the user's WinBoat"
- "never return secrets"
- "safe live writes only (read-only checks, recorded runs, containers without GPU, short stress runs within
  firmware limits)"

## How a machine change runs

1. A read runs in the operator as you and changes nothing. Some values (fan RPM, throttle reasons) stay `unknown`
   without root.
2. A change (class `machine`) goes over D-Bus to the root daemon. polkit shows a dialog on this machine. Only the human
   can approve it; the agent cannot.
3. The operator plans first. A refusal (a wrong value, a missing boot takeover) comes before the dialog, with nothing
   changed.
4. The daemon writes, reads back, and records an event with the old values. `undo <event>` turns it back.
5. So: read freely. For a change, give the user the exact command and its `--dry-run` output. The user runs it.
6. Test a change against a recording (`record`, `replay`) or in the e2e tests, not on the live firmware.
7. Do not use a GPU (`--gpu`, `gpu give`, `vfio bind`) live without the user's agreement.
8. Do not use `fan lock`. It is a raw EC write, and it is not available on this machine.
9. A `power` layer must also set its knobs for the other power source (or a layer without `power`), else the check
   refuses it.
10. Some changes apply at the next login or the next start, not now. The changes reference says when.

## How to check a change

1. `knobs show <knob>`: the layer chain and the history of one knob.
2. `plan`: the writes the daemon would make now. An empty write list means the machine is as the layers want.
3. `doctor`: the findings after the change.
4. `bench list`, `bench compare <earlier> <later>`: scores before and after.
5. Write the event, the values before and after, and the output into the dossier's EVIDENCE.

## Refs

- `refs/reference-reads.md` - read to find the command that shows a value.
- `refs/reference-changes.md` - read before you propose a change: what it changes and when it applies.
- `refs/scenario-lower-cpu-limit.md` - read when the user wants a different power limit.
- `refs/scenario-display-finding.md` - read when `display status` or `doctor` shows a display finding.
