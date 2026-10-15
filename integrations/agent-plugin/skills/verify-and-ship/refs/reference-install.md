# The checked install

`make install-package` installs legionctl on this machine. The user decided the form on 2026-10-15.

## After the first install

1. The Makefile runs `sudo -n /usr/lib/legionctl/legionctl-install`. It needs no password:
   `/etc/sudoers.d/legionctl-install` permits this one command, with no arguments, to the group `wheel`.
2. The wrapper reads no input from the caller. The GitHub repository `ksquarekumar/lenovolegionlinux` is fixed in it.
3. As the user `legionctl-build`, it reads the SHA of `main` on GitHub (`git ls-remote`).
4. It reads the runs of `ci.yml` for that SHA (branch `main`, event `push`) from the GitHub API. Without a passed run,
   it refuses and gives the reason (`no CI run`, `CI run completed failure`, `CI run in_progress`).
5. `legionctl-build` fetches the SHA into `/var/lib/legionctl-build/src`, checks out exactly that SHA (it refuses when
   `HEAD` is an other commit), and runs `makepkg` with `BUILDDIR` and `PKGDEST` in its home. The cargo, rustup and
   target caches stay in that home (mode 0700). Your processes cannot write there.
6. Root installs that package file with `pacman -U`, then restarts your operator
   (`systemctl --user -M $SUDO_USER@ restart legionctl-operator.service`).
7. Each step goes to the journal: `journalctl -t legionctl-install`.
8. A commit only on your branch or in your folder is never installed. Merge and push it, wait for CI, then install.
   A commit that changes only `*.md` or `docs/design/` has no CI run (`paths-ignore`): the wrapper refuses it.

## The first install

1. The package that brings the wrapper is not installed yet, so `make install-package` builds this checkout's package
   (`make package`) and installs it with `sudo pacman -U` (password). The user does this one time.
2. pacman then makes the user `legionctl-build` (`sysusers.d`, group `docker`) and its home (`tmpfiles.d`).
3. From then on, the Makefile uses only the wrapper.

## Limits

1. Agents write the code on `main`. The wrapper refuses only a commit that is not on `main` or that has no passed CI
   run. It does not review the code.
2. `legionctl-build` is in the group `docker` (`make static` builds the musl client in a container). That is equal to
   root for that user. It builds only commits that the wrapper accepted, and root installs those packages anyway.
3. The sudo timestamp of this machine (`timestamp_type=global`, 20 minutes) stays as the user set it.
