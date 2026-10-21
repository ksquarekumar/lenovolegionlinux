# `make ci-local`: a machine shaped like the GitHub runner of .github/workflows/ci.yml (job lint): Ubuntu 24.04, its
# packages, bubblewrap 0.12.0, the pinned tools, a user `runner` with the host user's ids (the checkout and the cargo
# registry are the host's), and no user systemd.
FROM ubuntu:24.04
ARG UID=1000
ARG GID=1000
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential pkg-config curl ca-certificates git python3 sudo make xz-utils unzip procps systemd docker.io \
      libudev-dev dbus bubblewrap clang jq libarchive-tools qemu-utils strace ripgrep libfontconfig-dev \
      libxkbcommon-x11-dev libxcb1-dev libwayland-dev libvulkan-dev meson ninja-build libcap-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sLO https://github.com/containers/bubblewrap/releases/download/v0.12.0/bubblewrap-0.12.0.tar.xz \
    && echo "9760d007363e3abba7c747489910f9f82d9fca53ba3bd3282e396fa3c97a3314  bubblewrap-0.12.0.tar.xz" | sha256sum -c \
    && tar xJf bubblewrap-0.12.0.tar.xz \
    && meson setup bubblewrap-0.12.0/build bubblewrap-0.12.0 --prefix=/usr -Dman=disabled -Dtests=false -Dselinux=disabled \
    && ninja -C bubblewrap-0.12.0/build install && rm -rf bubblewrap-0.12.0*
RUN userdel -r ubuntu && groupadd -g "$GID" runner && useradd -m -u "$UID" -g "$GID" runner \
    && echo 'runner ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/runner
USER runner
ENV PATH=/home/runner/.cargo/bin:/home/runner/.local/bin:/home/runner/.bun/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy,rustfmt \
    && curl -LsSf https://astral.sh/uv/install.sh | sh \
    && curl -fsSL https://bun.sh/install | bash \
    && cargo install --locked cargo-machete@0.9.2 cargo-deny@0.20.2 \
    && rm -rf /home/runner/.cargo/registry
# the runner image has the sqlite3 command (a test fills a database with it), docker's compose and buildx plugins and
# Python's jsonschema (a test checks a settings file against its schema)
USER root
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends sqlite3 \
      docker-compose-v2 docker-buildx python3-jsonschema \
    && rm -rf /var/lib/apt/lists/*
USER runner
