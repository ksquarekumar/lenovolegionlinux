#!/bin/sh
# `make ci-local` inside the runner-like container (tests/ci-local/Dockerfile), as root: the cgroup of the job as in
# .github/workflows/ci.yml (the runner user owns /sys/fs/cgroup/ci and the job runs in it), then the job's tools and
# `make ci` as that user. A test that runs past its time limit stops the run (tests/common/scratch.rs).
set -eu
mkdir -p /sys/fs/cgroup/ci
chown -R runner:runner /sys/fs/cgroup/ci
echo $$ >/sys/fs/cgroup/ci/cgroup.procs
# the tools' cache is a docker volume, made by root
chown runner:runner /home/runner/.cache
exec sudo -u runner --preserve-env=CARGO_TARGET_DIR,LEGIONCTL_TEST_MINUTES \
  env HOME=/home/runner PATH="$PATH" sh -c 'make tools && make ci'
