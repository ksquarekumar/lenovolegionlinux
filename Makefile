PREFIX ?= /usr
SYSCONFDIR ?= /etc
UNITDIR ?= $(PREFIX)/lib/systemd/system
USERUNITDIR ?= $(PREFIX)/lib/systemd/user
DATADIR ?= $(PREFIX)/share/legionctl
LIBDIR ?= $(PREFIX)/lib/legionctl
BINDIR ?= $(PREFIX)/bin
PROFILE ?= profiles/lenovo-82wq-legion-pro-7-16irx8h.json
EXTENSION = legionctl-osd@legionctl
PRE_COMMIT ?= pre-commit

.PHONY: help
help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-10s %s\n", $$1, $$2}'

.PHONY: tools
tools: ## install dev tooling (pre-commit; hooks fetch shellcheck, shfmt, actionlint)
	@command -v uv >/dev/null || { echo "uv is required: https://docs.astral.sh/uv/getting-started/installation/"; exit 1; }
	uv tool install --upgrade pre-commit
	$(PRE_COMMIT) install-hooks

.PHONY: hooks
hooks: tools ## install the git pre-commit hook
	$(PRE_COMMIT) install

.PHONY: fmt
fmt: ## format rust and shell in place
	cargo fmt --all
	-$(PRE_COMMIT) run shfmt --all-files

.PHONY: lint
lint: ## run every pre-commit hook on all files
	$(PRE_COMMIT) run --all-files --show-diff-on-failure

.PHONY: rust-lint
rust-lint: ## rustfmt check and clippy with warnings as errors
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: build
build: ## build legionctl, legionctl-system and legionctl-operator (release)
	cargo build --release --locked

.PHONY: validate
validate: ## check the profile's structure and ac/battery consistency (no hardware access)
	cargo run -q --locked -p legionctl -- validate --profile $(PROFILE)

.PHONY: test
test: ## end-to-end and behavior tests (no hardware access)
	cargo test --workspace --locked

.PHONY: ci
ci: lint rust-lint validate test ## what CI runs

.PHONY: package
package: ## build and install the Arch package (pacman tracks every file)
	cd packaging/arch && makepkg -sfi --noconfirm

.PHONY: lint-pkg
lint-pkg: ## namcap the PKGBUILD (Arch only)
	namcap packaging/arch/PKGBUILD

.PHONY: install
install: ## install into DESTDIR; the PKGBUILD calls this (install on this machine with `make package`)
	@test -x target/release/legionctl || { echo "target/release/legionctl missing: run 'make build' first"; exit 1; }
	install -Dm755 target/release/legionctl "$(DESTDIR)$(BINDIR)/legionctl"
	install -Dm755 target/release/legionctl-system "$(DESTDIR)$(LIBDIR)/legionctl-system"
	install -Dm755 target/release/legionctl-operator "$(DESTDIR)$(LIBDIR)/legionctl-operator"
	install -Dm644 $(PROFILE) "$(DESTDIR)$(DATADIR)/profiles/$(notdir $(PROFILE))"
	install -d "$(DESTDIR)$(DATADIR)" && cp -r integrations/agent-plugin "$(DESTDIR)$(DATADIR)/"
	install -Dm644 systemd/legionctl-system.service "$(DESTDIR)$(UNITDIR)/legionctl-system.service"
	install -Dm644 systemd/user/legionctl-operator.service "$(DESTDIR)$(USERUNITDIR)/legionctl-operator.service"
	install -Dm644 systemd/power-profiles-daemon.service.d/10-legionctl.conf "$(DESTDIR)$(UNITDIR)/power-profiles-daemon.service.d/10-legionctl.conf"
	install -Dm644 dbus/org.legionctl.System1.conf "$(DESTDIR)$(PREFIX)/share/dbus-1/system.d/org.legionctl.System1.conf"
	install -Dm644 polkit/org.legionctl.policy "$(DESTDIR)$(PREFIX)/share/polkit-1/actions/org.legionctl.policy"
	install -d "$(DESTDIR)$(PREFIX)/share/gnome-shell/extensions/$(EXTENSION)"
	install -m644 gnome-extension/$(EXTENSION)/* "$(DESTDIR)$(PREFIX)/share/gnome-shell/extensions/$(EXTENSION)/"
	install -Dm644 tuning/scx_loader.toml "$(DESTDIR)$(SYSCONFDIR)/scx_loader.toml"
