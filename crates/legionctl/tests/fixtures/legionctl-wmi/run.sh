#!/bin/bash
# The legionctl-wmi test in the guest (crates/legionctl/tests/e2e.rs). QEMU gives the stand-in firmware (ssdt.asl)
# as an ACPI table, so the WMI devices exist from the start, as on the real machine. First start: build and install
# the module with DKMS, install the package's blacklist and restart. Second start: udev loaded the drivers for the
# WMI devices, and the test uses the module through sysfs only. Each check writes "ok <name>" or "FAIL <name>: <why>"
# to the results disk (serial lwmiresults); the test reads it after power-off.
set -u
here=$(dirname "$0")
results=/var/lib/lwmi-results

finish() {
  {
    echo "kernel log:"
    dmesg | grep -v 'ACPI Debug' | tail -n 40
    echo "done"
  } >>"$results"
  dd if="$results" of=/dev/disk/by-id/virtio-lwmiresults conv=notrunc status=none
  sync
}

ok() { echo "ok $1" >>"$results"; }
fail() { echo "FAIL $1: $2" >>"$results"; }
expect() { # expect <name> <expected> <actual>
  if [ "$2" = "$3" ]; then ok "$1"; else fail "$1" "expected '$2', got '$3'"; fi
}
# write <file> <value>: ok, EBUSY, EINVAL or the error text
write() {
  local err
  if err=$(bash -c "echo '$2' > '$1'" 2>&1); then
    echo ok
  else
    case "$err" in
      *busy*) echo EBUSY ;;
      *Invalid*) echo EINVAL ;;
      *) echo "$err" ;;
    esac
  fi
}
# the bytes of the last "LWMI <call>" Debug line (ToHexString: 0x.. separated), as plain hex
last_call() {
  dmesg | grep "LWMI $1 " | tail -n 1 | python3 -c "
import re, sys
line = sys.stdin.read().split('LWMI $1 ', 1)[-1]
print(''.join('%02x' % int(t, 16) for t in re.findall(r'(?:0x)?([0-9A-Fa-f]{1,2})\b', line)))"
}
calls() { dmesg | grep -c "LWMI $1 "; }
wait_for() { # wait_for <path>: up to 20 s
  local i
  for i in $(seq 1 40); do
    [ -e "$1" ] && return 0
    sleep 0.5
  done
  return 1
}
dmesg -n 1

if [ ! -e "$results" ]; then
  uname -r >"$results"
  version=$(sed -n 's/^PACKAGE_VERSION="\(.*\)"/\1/p' "$here/legionctl-wmi/dkms.conf")
  cp -r "$here/legionctl-wmi" "/usr/src/legionctl-wmi-$version"
  if dkms install "legionctl-wmi/$version" -k "$(uname -r)" >/tmp/dkms.log 2>&1; then
    ok dkms-build
  else
    fail dkms-build "$(
      tail -n 30 /tmp/dkms.log
      tail -n 30 "/var/lib/dkms/legionctl-wmi/$version/build/make.log" 2>/dev/null
    )"
    finish
    exit 1
  fi
  install -m 644 "$here/legionctl-wmi.conf" /etc/modprobe.d/legionctl-wmi.conf
  # the restart stops this script before the unit powers off
  systemctl reboot
  sleep infinity
fi
trap finish EXIT

# the blacklist kept the upstream drivers away; udev loaded ours for the WMI devices at start
fw=/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes
if ! wait_for "$fw"; then
  fail bind "no $fw; WMI devices: $(ls /sys/bus/wmi/devices 2>&1); $(lsmod | grep -e wmi -e legionctl); $(dmesg | grep -i -e wmi -e legionctl | tail -n 20)"
  exit 1
fi
udevadm settle
expect only-legionctl-wmi "legionctl_wmi" "$(lsmod | awk '/^(legionctl_wmi|lenovo_wmi)/ {print $1}' | sort | tr '\n' ' ' | sed 's/ $//')"
# the helper that calls the stand-in's Fn+Q method, as the EC does
cp -r "$here/aml-call" /tmp/aml-call
if ! make -C "/lib/modules/$(uname -r)/build" M=/tmp/aml-call modules >/tmp/aml-call.log 2>&1; then
  fail aml-call "$(tail -n 20 /tmp/aml-call.log)"
fi
echo 1 >/sys/module/acpi/parameters/aml_debug_output

# the class device takes custom (the legacy file does not); the daemon writes it too (profile paths.platform_profile)
profile=$(grep -l '^lenovo-wmi-gamezone$' /sys/class/platform-profile/*/name | head -n 1 | xargs dirname)/profile
legacy=/sys/firmware/acpi/platform_profile
expect profile-choices "low-power balanced performance max-power custom" "$(cat /sys/firmware/acpi/platform_profile_choices)"
expect profile-start "balanced" "$(cat "$profile")"
expect profile-set "ok performance" "$(write "$profile" performance) $(cat "$profile")"
expect profile-call "0300000000000000" "$(last_call GZ44)"

# firmware attributes: the bounds of Capability Data 01, and only the supported ones
expect fw-attributes "cpu_temp gpu_nv_ctgp gpu_nv_ppab gpu_temp ppt_cpu_cl ppt_pl1_spl ppt_pl1_tau ppt_pl2_sppt" \
  "$(find "$fw" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ' | sed 's/ $//')"
spl=$fw/ppt_pl1_spl
expect fw-bounds "30 140 70 1 integer" \
  "$(cat "$spl"/min_value "$spl"/max_value "$spl"/default_value "$spl"/scalar_increment "$spl"/type | tr '\n' ' ' | sed 's/ $//')"
expect fw-display "Set the CPU sustained power limit" "$(cat "$spl"/display_name)"
expect fw-not-custom "EBUSY" "$(write "$spl"/current_value 100)"
if dmesg | grep -q 'ppt_pl1_spl works in custom mode only; the mode is performance'; then ok fw-not-custom-warning; else
  fail fw-not-custom-warning "no warning that names the mode"
fi
write "$profile" custom >/dev/null
expect fw-above-max "EINVAL" "$(write "$spl"/current_value 141)"
expect fw-below-min "EINVAL" "$(write "$spl"/current_value 29)"
expect fw-set "ok 100" "$(write "$spl"/current_value 100) $(cat "$spl"/current_value)"
expect fw-call "00ff020164000000" "$(last_call OM18)"
write "$profile" balanced >/dev/null
expect fw-value-of-mode "90" "$(cat "$spl"/current_value)"

hwmon=$(grep -l '^legionctl_wmi$' /sys/class/hwmon/hwmon*/name | head -n 1 | xargs dirname)
expect fan-rpm "2200 2300" "$(cat "$hwmon/fan1_input" "$hwmon/fan2_input" | tr '\n' ' ' | sed 's/ $//')"

# the fan table: levels 1-10, custom mode only, written as the DSDT reads it, read back
point() { echo "$hwmon/pwm1_auto_point$1_pwm"; }
table() { for i in $(seq 1 10); do cat "$(point "$i")"; done | tr '\n' ' ' | sed 's/ $//'; }
expect table-not-custom "EBUSY" "$(write "$(point 1)" 2)"
if dmesg | grep -q 'the fan table works in custom mode only; the mode is balanced'; then ok table-not-custom-warning; else
  fail table-not-custom-warning "no warning that names the mode"
fi
write "$profile" custom >/dev/null
expect table-set "ok" "$(write "$(point 1)" 2)"
expect table-read "2 2 3 4 5 6 7 8 9 10" "$(table)"
expect table-bytes \
  "0000000000000200020003000400050006000700080009000a000000000000000000000000000000000000000000000000000000000000000000000000000000" \
  "$(last_call FAN6)"
expect table-below-minimum "EINVAL" "$(write "$(point 9)" 2)"
expect table-above-10 "EINVAL" "$(write "$(point 10)" 11)"
expect table-overflow "EINVAL" "$(write "$(point 10)" 300)"
expect table-decreasing "EINVAL" "$(write "$(point 3)" 1)"
expect table-not-a-number "EINVAL" "$(write "$(point 5)" abc)"
expect table-unchanged "2 2 3 4 5 6 7 8 9 10" "$(table)"

# the firmware resets the table at each mode change; the module writes it again on entry to custom mode
write "$profile" balanced >/dev/null
write "$profile" custom >/dev/null
expect table-again-on-custom "2 2 3 4 5 6 7 8 9 10" "$(table)"

# Fn+Q: the firmware enters custom mode and sends the event; userspace gets the change, the table comes back
write "$profile" balanced >/dev/null
python3 -c "
import select, sys
f = open('$legacy')
f.read()
p = select.poll()
p.register(f, select.POLLPRI | select.POLLERR)
sys.exit(0 if p.poll(10000) else 1)
" &
poller=$!
sleep 1
insmod /tmp/aml-call/aml_call.ko method='\_SB.GZFD.FNQ' && rmmod aml_call
if wait "$poller"; then ok fnq-notify; else fail fnq-notify "no change notice on $legacy"; fi
expect fnq-profile "custom" "$(cat "$profile")"
sleep 1
expect fnq-table-again "2 2 3 4 5 6 7 8 9 10" "$(table)"

# full speed: pwm1_enable 0, custom mode only; it ends with custom mode
expect full-speed-off "2" "$(cat "$hwmon/pwm1_enable")"
expect full-speed-on "ok 0" "$(write "$hwmon/pwm1_enable" 0) $(cat "$hwmon/pwm1_enable")"
expect full-speed-call "0000020401000000" "$(last_call OM18)"
expect full-speed-manual "EINVAL" "$(write "$hwmon/pwm1_enable" 1)"
write "$profile" balanced >/dev/null
expect full-speed-ends "2" "$(cat "$hwmon/pwm1_enable")"
expect full-speed-not-custom "EBUSY" "$(write "$hwmon/pwm1_enable" 0)"

# fan max mode: one byte, any mode, kept by the module
expect fan-max-on "ok 1" "$(write "$hwmon/fan_max_mode" 1) $(cat "$hwmon/fan_max_mode")"
expect fan-max-call "01" "$(last_call GZ13)"

# resume: fan max mode and, in custom mode, the table again
write "$profile" custom >/dev/null
max_before=$(calls GZ13)
table_before=$(calls FAN6)
if [ -e /sys/power/pm_test ]; then
  echo devices >/sys/power/pm_test
  echo freeze >/sys/power/state
  echo none >/sys/power/pm_test
  expect resume-fan-max "$((max_before + 1)) 01" "$(calls GZ13) $(last_call GZ13)"
  expect resume-table "$((table_before + 1))" "$(calls FAN6)"
else
  fail resume "no /sys/power/pm_test (CONFIG_PM_DEBUG)"
fi

# the mute LEDs of the Lenovo Utility interface
expect led-micmute "ok 01000000" "$(write /sys/class/leds/platform::micmute/brightness 1) $(last_call SK2)"
expect led-mute "ok 04000000" "$(write /sys/class/leds/platform::mute/brightness 1) $(last_call SK2)"

# unload: the hub goes first (device links), nothing is left, the kernel logs no warning
if modprobe -r legionctl_wmi && [ ! -e /sys/class/firmware-attributes/lenovo-wmi-other-0 ]; then ok unload; else
  fail unload "the module or its firmware attributes stay"
fi
expect kernel-clean "" "$(dmesg | grep -E 'BUG|WARNING:|Oops|refcount_t|list_del corruption')"
