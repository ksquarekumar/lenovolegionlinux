#!/bin/sh
# The test gate for commits made outside `legionctl workspace checkpoint` (design 23, A7): the new and changed tests
# of the staged change set need an assertion and a recorded kill, or an accept.
set -eu
# nothing staged (`pre-commit run --all-files`, as in `make lint` and CI): no change set to review
git diff --cached --quiet && exit 0
if ! legionctl code tests review --help >/dev/null 2>&1; then
  echo "test gate: the legionctl on PATH has no 'code tests review'; install the new package (make install-package)" >&2
  exit 1
fi
exec legionctl code tests review --staged
