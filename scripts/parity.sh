#!/bin/bash
# Parity gate (docs/design/07-plan.md): the Rust and Python tools must plan the same writes and report the same
# findings on this machine. Dry runs and read-only commands only; RAPL lines are left out because the Rust port does
# not write RAPL; Python doctor also prints its status first, so only its findings are compared. Deleted in the switch.
set -u
rust=${1:-target/debug/legion}
status=0
for c in "apply ac" "apply battery" "apply auto" "apply auto --keep-override" "reassert ac" "reassert battery" "follow" \
  "gpu status" "gpu primary nvidia --dry-run" "cleanup --dry-run" "capabilities" "doctor"; do
  read -ra args <<<"$c"
  dry=(--dry-run)
  case "$c" in gpu\ status | capabilities | doctor | *--dry-run) dry=() ;; esac
  py=$(python3 -u src/legionctl/legionctl "${args[@]}" "${dry[@]}" 2>&1 | grep -v -e powercap -e RAPL)
  [ "$c" = doctor ] && py=$(echo "$py" | sed -n '/^doctor:/,$p')
  rs=$("$rust" "${args[@]}" "${dry[@]}" 2>&1)
  if [ "$py" = "$rs" ]; then
    echo "same: $c"
  else
    echo "DIFFERENT: $c"
    diff <(echo "$py") <(echo "$rs")
    status=1
  fi
done
exit $status
