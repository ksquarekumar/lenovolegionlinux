#!/usr/bin/env bash
set -euo pipefail

duration=0
output_dir=""

usage() {
  cat <<'EOF'
Usage: sudo -E ./scripts/capture-capabilities.sh [--duration SECONDS] [--output DIR]

Captures Legion hotkey, WMI, display, fan, thermal, and power evidence.

Interactive mode is the default. The script prompts for Fn+Q, Fn+R, Fn+Space,
and Fn+Esc, then brackets each key press with timestamp markers.

Options:
  --duration SECONDS  Record monitors for a fixed duration instead of prompts.
  --output DIR        Write capture files to DIR.
  -h, --help          Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration)
      duration="${2:?missing value for --duration}"
      shift 2
      ;;
    --output)
      output_dir="${2:?missing value for --output}"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${output_dir}" ]]; then
  output_dir="/tmp/legion-capability-capture-$(date +%Y%m%d-%H%M%S)"
fi

mkdir -p "${output_dir}"

markers="${output_dir}/markers.log"
monitors="${output_dir}/monitors.log"
pids=()

log() {
  printf '[%s] %s\n' "$(date --iso-8601=seconds)" "$*"
}

mark() {
  printf '%s\t%s\n' "$(date --iso-8601=ns)" "$*" | tee -a "${markers}"
}

have() {
  command -v "$1" >/dev/null 2>&1
}

run_cmd() {
  local outfile="$1"
  shift
  {
    printf '$'
    printf ' %q' "$@"
    printf '\n\n'
    "$@"
  } >"${outfile}" 2>&1 || true
}

read_file() {
  local path="$1"
  if [[ -r "${path}" ]]; then
    printf '%s: ' "${path}"
    tr -d '\000' <"${path}" 2>/dev/null | head -c 500 || true
    printf '\n'
  fi
}

snapshot_sysfs_values() {
  local outfile="$1"
  {
    echo "== DMI =="
    for path in \
      /sys/class/dmi/id/sys_vendor \
      /sys/class/dmi/id/product_name \
      /sys/class/dmi/id/product_family \
      /sys/class/dmi/id/product_version \
      /sys/class/dmi/id/product_sku \
      /sys/class/dmi/id/board_name \
      /sys/class/dmi/id/bios_version \
      /sys/class/dmi/id/bios_date \
      /sys/class/dmi/id/bios_release \
      /sys/class/dmi/id/ec_firmware_release; do
      read_file "${path}"
    done

    echo
    echo "== Power supply =="
    for path in /sys/class/power_supply/*/{type,online,status,power_now,voltage_now,current_now}; do
      read_file "${path}"
    done

    echo
    echo "== Platform profile =="
    read_file /sys/firmware/acpi/platform_profile
    read_file /sys/firmware/acpi/platform_profile_choices

    echo
    echo "== Intel pstate =="
    for path in \
      /sys/devices/system/cpu/intel_pstate/status \
      /sys/devices/system/cpu/intel_pstate/no_turbo \
      /sys/devices/system/cpu/intel_pstate/min_perf_pct \
      /sys/devices/system/cpu/intel_pstate/max_perf_pct \
      /sys/module/processor/parameters/ignore_ppc; do
      read_file "${path}"
    done

    echo
    echo "== CPU policies =="
    for policy in /sys/devices/system/cpu/cpufreq/policy*; do
      [[ -d "${policy}" ]] || continue
      read_file "${policy}/scaling_driver"
      read_file "${policy}/scaling_governor"
      read_file "${policy}/energy_performance_preference"
      read_file "${policy}/scaling_min_freq"
      read_file "${policy}/scaling_max_freq"
    done

    echo
    echo "== RAPL package zones =="
    for zone in /sys/class/powercap/intel-rapl*:*; do
      [[ -d "${zone}" ]] || continue
      if [[ "$(cat "${zone}/name" 2>/dev/null || true)" != "package-0" ]]; then
        continue
      fi
      echo "-- ${zone} --"
      for path in "${zone}"/constraint_*_{name,power_limit_uw,time_window_us}; do
        read_file "${path}"
      done
    done

    echo
    echo "== VPC2004 fan_mode paths =="
    find /sys/devices -path '*VPC2004:00/fan_mode' -type f -print 2>/dev/null | while read -r path; do
      read_file "${path}"
    done

    echo
    echo "== WMI devices =="
    find /sys/bus/wmi/devices -maxdepth 3 -type f \( -name modalias -o -name uevent \) -print 2>/dev/null | while read -r path; do
      read_file "${path}"
    done

    echo
    echo "== LEDs =="
    find /sys/class/leds -maxdepth 2 -type f \( -name brightness -o -name max_brightness -o -name trigger \) -print 2>/dev/null | while read -r path; do
      read_file "${path}"
    done

    echo
    echo "== Input device names =="
    find /sys/class/input -maxdepth 4 -type f \( -name name -o -name phys -o -name uniq \) -print 2>/dev/null | while read -r path; do
      read_file "${path}"
    done

    echo
    echo "== Modules =="
    awk '{print $1}' /proc/modules | grep -E '^(ideapad_laptop|lenovo_wmi|lenovo_wmi_gamezone|lenovo_wmi_events|thinkpad_acpi|legion_laptop|lenovo_legion|lenovo_legion_wmi)$' || true
  } >"${outfile}" 2>&1
}

snapshot_phase() {
  local phase="$1"
  local dir="${output_dir}/${phase}"
  mkdir -p "${dir}"

  snapshot_sysfs_values "${dir}/sysfs.txt"

  run_cmd "${dir}/uname.txt" uname -a
  if have powerprofilesctl; then
    run_cmd "${dir}/powerprofilesctl.txt" powerprofilesctl
  fi
  if have nvidia-smi; then
    run_cmd "${dir}/nvidia-power.txt" nvidia-smi -q -d POWER
    run_cmd "${dir}/nvidia-query.txt" nvidia-smi --query-gpu=power.draw,power.limit,power.default_limit,power.min_limit,power.max_limit,clocks.gr,clocks.mem,temperature.gpu --format=csv
  fi
  if have sensors; then
    run_cmd "${dir}/sensors.txt" sensors
  fi
  if [[ -r /proc/acpi/ibm/fan ]]; then
    run_cmd "${dir}/thinkpad-acpi-fan.txt" cat /proc/acpi/ibm/fan
  fi

  if have kscreen-doctor; then
    run_cmd "${dir}/display-kscreen-doctor.txt" kscreen-doctor -o
  fi
  if have wlr-randr; then
    run_cmd "${dir}/display-wlr-randr.txt" wlr-randr
  fi
  if have xrandr; then
    run_cmd "${dir}/display-xrandr.txt" xrandr --query
  fi
  if have hyprctl; then
    run_cmd "${dir}/display-hyprctl.txt" hyprctl monitors all
  fi
  if have swaymsg; then
    run_cmd "${dir}/display-swaymsg.txt" swaymsg -t get_outputs
  fi
}

start_monitor() {
  local name="$1"
  shift
  local cmd="$1"
  local outfile="${output_dir}/${name}.log"
  if ! have "${cmd}"; then
    echo "missing command: ${cmd}" >"${outfile}"
    return 0
  fi
  log "starting monitor ${name}: $*"
  "$@" >"${outfile}" 2>&1 &
  pids+=("$!")
  printf '%s\t%s\t%s\n' "$!" "${name}" "$*" >>"${monitors}"
}

cleanup() {
  local pid
  for pid in "${pids[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
}

trap cleanup EXIT INT TERM

if [[ "${EUID}" -ne 0 ]]; then
  log "warning: not running as root; libinput, journal, and some sysfs data may be incomplete"
fi

log "writing capture to ${output_dir}"
mark "capture-start"
snapshot_phase "before"

start_monitor "journal-kernel" journalctl -kf -o short-precise
start_monitor "udev-monitor" udevadm monitor --kernel --udev --property
start_monitor "acpi-listen" acpi_listen
start_monitor "libinput-debug-events" libinput debug-events --show-keycodes

if [[ "${duration}" =~ ^[0-9]+$ ]] && [[ "${duration}" -gt 0 ]]; then
  mark "fixed-duration-start duration=${duration}s"
  sleep "${duration}"
  mark "fixed-duration-end"
else
  cat <<'EOF'

Interactive capture:
For each prompt, press Enter, then press the listed hotkey once within the
5-second window. Wait for the next prompt before pressing the next hotkey.

EOF
  for key in "Fn+Q" "Fn+R" "Fn+Space" "Fn+Esc"; do
    read -r -p "Press Enter to open the 5-second window for ${key}: "
    mark "window-start ${key}"
    sleep 5
    mark "window-end ${key}"
    snapshot_phase "after-${key//+/plus}"
  done
fi

snapshot_phase "after"
mark "capture-end"
cleanup

log "capture complete: ${output_dir}"
log "key files:"
log "  ${markers}"
log "  ${output_dir}/journal-kernel.log"
log "  ${output_dir}/udev-monitor.log"
log "  ${output_dir}/libinput-debug-events.log"
