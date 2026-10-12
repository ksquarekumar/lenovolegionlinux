#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${EUID}" -ne 0 ]]; then
  echo "Run as root: sudo ${repo_root}/scripts/install-apply-check.sh" >&2
  exit 1
fi

make -C "${repo_root}" install
udevadm control --reload
systemctl daemon-reload

legionctl cleanup
systemctl enable legion-apply.service
systemctl enable --now legion-reassert-ac.timer
legionctl apply auto

echo
echo "== legionctl doctor =="
legionctl doctor || true

echo
echo "== nvidia power =="
nvidia-smi -q -d POWER || true

echo
echo "== fan and thermal =="
sensors || true
