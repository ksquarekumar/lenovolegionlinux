#!/usr/bin/env bash
# Measure CPU power-limit (RAPL) settings for throughput and thermals, CPU-only and with the GPU pegged.
#
# Default run (~60 min, AC power, idle machine):
#   sustained  PL1 sweep x {cpu, gpu}: all-core stress-ng for --duration (past the ~56 s PL1 window),
#              averages over the second half; "gpu" runs gpu-burn alongside and records GPU power/clock/temp
#              and Gflop/s, so a CPU limit that steals the shared cooling/power budget from the GPU shows up.
#   burst      PL2 sweep: --burst seconds from a cooled state at the current PL1 (what compiles and app
#              launches see). PL4 (peak_power) still caps instantaneous power.
# Ends with a recommendation: the knee is the smallest PL1 within 2% of the best score in each scenario.
#
# Usage: sudo ./scripts/rapl-sweep.sh [--pl1 "55 70 85 100 115"] [--pl2 "90 110 125"] [--scenarios "cpu gpu"]
#        [--duration 180] [--burst 20] [--cooldown 90] [--gpu-cmd CMD] [--out FILE] [--force]
set -euo pipefail
[[ $EUID -eq 0 ]] || {
  echo "run with sudo" >&2
  exit 1
}

user_home=$(getent passwd "${SUDO_USER:-root}" | cut -d: -f6)
pl1_list="55 70 85 100 115"
pl2_list="90 110 125"
scenarios="cpu gpu"
duration=180
burst=20
cooldown=90
gpu_cmd=""
[[ -x $user_home/.local/bin/gpu-burn ]] && gpu_cmd="$user_home/.local/bin/gpu-burn 100000"
out="rapl-sweep-$(date +%Y%m%d-%H%M%S).csv"
force=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --pl1)
      pl1_list=$2
      shift 2
      ;;
    --pl2)
      pl2_list=$2
      shift 2
      ;;
    --scenarios)
      scenarios=$2
      shift 2
      ;;
    --duration)
      duration=$2
      shift 2
      ;;
    --burst)
      burst=$2
      shift 2
      ;;
    --cooldown)
      cooldown=$2
      shift 2
      ;;
    --gpu-cmd)
      gpu_cmd=$2
      shift 2
      ;;
    --out)
      out=$2
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done
for c in stress-ng turbostat nvidia-smi; do command -v $c >/dev/null || {
  echo "missing $c" >&2
  exit 1
}; done
[[ $scenarios == *gpu* && -z $gpu_cmd ]] && {
  echo "gpu scenario needs gpu-burn or --gpu-cmd (or --scenarios cpu)" >&2
  exit 1
}
[[ $(cat /sys/class/power_supply/ADP0/online 2>/dev/null) == 1 ]] || {
  echo "plug in AC first" >&2
  exit 1
}

# background load (a VM install, a build) skews every number; measure on an idle machine
idle_check() {
  local load
  load=$(cut -d' ' -f1 /proc/loadavg)
  if awk -v l="$load" 'BEGIN{exit !(l > 2)}'; then
    echo "load average is $load; stop other work first (or pass --force)" >&2
    [[ $force == 1 ]] || exit 1
  fi
}
idle_check

zones=()
for z in /sys/class/powercap/intel-rapl:0 /sys/class/powercap/intel-rapl-mmio:0; do
  [[ -e $z/constraint_0_power_limit_uw ]] && zones+=("$z")
done
declare -A orig
for z in "${zones[@]}"; do
  orig["$z.0"]=$(<"$z/constraint_0_power_limit_uw")
  orig["$z.1"]=$(<"$z/constraint_1_power_limit_uw")
done
base_pl1=$((${orig["${zones[0]}.0"]} / 1000000))
base_pl2=$((${orig["${zones[0]}.1"]} / 1000000))

set_limits() { # pl1_w pl2_w
  for z in "${zones[@]}"; do
    echo $(($1 * 1000000)) >"$z/constraint_0_power_limit_uw"
    echo $(($2 * 1000000)) >"$z/constraint_1_power_limit_uw"
  done
}
reassert_was_active=0
systemctl is-active -q legion-reassert-ac.timer && reassert_was_active=1
restore() {
  trap - EXIT INT TERM # run once, even when INT is followed by EXIT
  for z in "${zones[@]}"; do
    echo "${orig["$z.0"]}" >"$z/constraint_0_power_limit_uw" || true
    echo "${orig["$z.1"]}" >"$z/constraint_1_power_limit_uw" || true
  done
  [[ -n ${gpu_pid:-} ]] && kill "$gpu_pid" 2>/dev/null || true
  [[ $reassert_was_active == 1 ]] && systemctl start legion-reassert-ac.timer || true
  echo "restored PL1=${base_pl1}W PL2=${base_pl2}W" >&2
}
trap restore EXIT INT TERM
# nothing else may write limits mid-measurement; also don't run `legionctl apply` while this runs
systemctl stop legion-reassert-ac.timer 2>/dev/null || true

tmp=$(mktemp -d)
# package-wide counter, mirrored under every cpu; read one copy
throttle_count() { cat /sys/devices/system/cpu/cpu0/thermal_throttle/package_throttle_count 2>/dev/null || echo 0; }

# measure SCENARIO SECONDS INTERVAL HALF -> sets: score pkg_w t_avg t_max mhz thr gpu_w gpu_mhz gpu_t gflops
measure() {
  local scen=$1 secs=$2 interval=$3 half=$4 ts_pid nv_pid thr0
  [[ $scen == gpu ]] && {
    bash -c "exec $gpu_cmd" >"$tmp/gpu" 2>&1 &
    gpu_pid=$!
    sleep 5 # let the GPU ramp before the CPU load starts
  }
  turbostat --quiet --show PkgWatt,PkgTmp,Bzy_MHz --interval "$interval" --num_iterations $((secs / interval)) >"$tmp/ts" 2>/dev/null &
  ts_pid=$!
  nvidia-smi --query-gpu=power.draw,clocks.sm,temperature.gpu --format=csv,noheader,nounits -lms $((interval * 1000)) >"$tmp/nv" 2>/dev/null &
  nv_pid=$!
  thr0=$(throttle_count)
  stress-ng --cpu 0 --cpu-method matrixprod -t "${secs}s" --metrics --yaml "$tmp/y.yaml" >/dev/null 2>&1
  wait "$ts_pid" || true
  kill "$nv_pid" 2>/dev/null || true
  wait "$nv_pid" 2>/dev/null || true
  thr=$(($(throttle_count) - thr0))
  gflops=""
  if [[ -n ${gpu_pid:-} ]]; then
    kill "$gpu_pid" 2>/dev/null || true
    wait "$gpu_pid" 2>/dev/null || true
    unset gpu_pid
    gflops=$(grep -oE '[0-9]+ Gflop/s' "$tmp/gpu" | tail -1 | cut -d' ' -f1 || true)
  fi
  score=$(awk '/bogo-ops-per-second-real-time/{printf "%.0f", $2; exit}' "$tmp/y.yaml")
  # turbostat repeats its header per sample; the first row after each header is the package summary
  read -r pkg_w t_avg t_max mhz < <(awk -v half="$half" '$1=="Bzy_MHz"{getline; rows[++n]=$0}
    END{h=half?int(n/2)+1:1; for(i=h;i<=n;i++){split(rows[i],f); m+=f[1]; t+=f[2]; if(f[2]>tm)tm=f[2]; w+=f[3]; c++}
        printf "%.1f %.1f %.0f %.0f\n", w/c, t/c, tm, m/c}' "$tmp/ts")
  read -r gpu_w gpu_mhz gpu_t < <(awk -F', *' -v half="$half" '{r[++n]=$0}
    END{h=half?int(n/2)+1:1; for(i=h;i<=n;i++){split(r[i],f); w+=f[1]; m+=f[2]; t+=f[3]; c++}
        if(c) printf "%.1f %.0f %.0f\n", w/c, m/c, t/c; else print "- - -"}' "$tmp/nv")
}

echo "phase,scenario,pl1_w,pl2_w,load1,score_ops_s,pkg_w,pkg_temp_avg_c,pkg_temp_max_c,busy_mhz,throttle_events,gpu_w,gpu_mhz,gpu_temp_c,gpu_gflops" | tee "$out"
row() { echo "$1,$2,$3,$4,$5,$score,$pkg_w,$t_avg,$t_max,$mhz,$thr,$gpu_w,$gpu_mhz,$gpu_t,${gflops:-}" | tee -a "$out"; }

for scen in $scenarios; do
  for pl1 in $pl1_list; do
    sleep "$cooldown"
    load1=$(cut -d' ' -f1 /proc/loadavg)
    set_limits "$pl1" "$base_pl2"
    measure "$scen" "$duration" 5 1
    row sustained "$scen" "$pl1" "$base_pl2" "$load1"
  done
done

for pl2 in $pl2_list; do
  sleep "$cooldown"
  load1=$(cut -d' ' -f1 /proc/loadavg)
  set_limits "$base_pl1" "$pl2"
  measure cpu "$burst" 1 0
  row burst cpu "$base_pl1" "$pl2" "$load1"
done
rm -rf "$tmp"

echo
echo "recommendation (knee = smallest limit within 2% of the best score):" >&2
awk -F, 'NR>1{k=$1"/"$2; if($6>best[k])best[k]=$6; s[k,++n[k]]=$0}
  END{for(k in n){split(k,p,"/"); lim=(p[1]=="burst")?4:3; knee=""
        for(i=1;i<=n[k];i++){split(s[k,i],f,","); if(f[6]>=0.98*best[k] && (knee=="" || f[lim]+0<knee+0)){knee=f[lim]; row=s[k,i]}}
        split(row,f,","); printf "  %-16s %s=%sW  score=%s  pkg=%sW  max=%sC  throttles=%s%s\n", k, (lim==4?"PL2":"PL1"), knee, f[6], f[7], f[9], f[11], (f[15]!=""?"  gpu="f[15]" Gflop/s @ "f[12]"W":"")}}' "$out" >&2
echo "wrote $out" >&2
