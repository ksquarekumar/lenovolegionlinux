#!/bin/bash
# Parity gate (docs/design/07-plan.md): the Rust and Python tools must plan the same writes on this machine.
# Dry runs only; RAPL lines are left out because the Rust port does not write RAPL. Deleted in the switch.
set -u
rust=${1:-target/debug/legion}
status=0
for c in "apply ac" "apply battery" "apply auto" "apply auto --keep-override" "reassert ac" "reassert battery" "follow"; do
  read -ra args <<<"$c"
  py=$(python3 -u src/legionctl/legionctl "${args[@]}" --dry-run 2>&1 | grep -v -e powercap -e RAPL)
  rs=$("$rust" "${args[@]}" --dry-run 2>&1)
  if [ "$py" = "$rs" ]; then
    echo "same: $c"
  else
    echo "DIFFERENT: $c"
    diff <(echo "$py") <(echo "$rs")
    status=1
  fi
done
exit $status
