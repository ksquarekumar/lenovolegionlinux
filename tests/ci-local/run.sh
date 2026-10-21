#!/bin/sh
# `make ci-local` inside the runner-like container (tests/ci-local/Dockerfile), as root: a cgroup for the job that the
# runner user owns, as .github/workflows/ci.yml makes /sys/fs/cgroup/ci (here below the container's own cgroup, which
# goes with the container), then the job's tools and `make ci` (CI_LOCAL_JOB) as that user. A test that runs past its
# time limit stops the run (tests/common/scratch.rs). The job's HOME is the host user's (HOST_HOME): `make static`
# gives the host's docker paths below it (the registry).
set -eu
own=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)
mkdir -p "$own/ci"
chown -R runner:runner "$own/ci"
echo $$ >"$own/ci/cgroup.procs"
# the host's docker socket, through its group
gid=$(stat -c %g /var/run/docker.sock)
getent group "$gid" >/dev/null || groupadd -g "$gid" hostdocker
usermod -aG "$(getent group "$gid" | cut -d: -f1)" runner
# the mounts made the folders above them as root
chown runner:runner "$HOST_HOME" "$HOST_HOME/.cache" "$HOST_HOME/.cargo"
# setpriv, not sudo: PAM would set the open files back to 1024 (the runner has 65536)
status=0
setpriv --reuid runner --regid runner --init-groups \
  env HOME="$HOST_HOME" RUSTUP_HOME=/home/runner/.rustup PATH="$HOST_HOME/.local/bin:$PATH" sh -c "$CI_LOCAL_JOB" ||
  status=$?
echo $$ >"$own/cgroup.procs"
rmdir "$own/ci" 2>/dev/null || echo "ci-local: $own/ci stays (a process of the job runs on)"
exit "$status"
