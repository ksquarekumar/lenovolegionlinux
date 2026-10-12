PREFIX ?= /usr
SYSCONFDIR ?= /etc
UNITDIR ?= $(PREFIX)/lib/systemd/system
UDEVDIR ?= $(SYSCONFDIR)/udev/rules.d
DATADIR ?= $(PREFIX)/share/legionctl
BINDIR ?= $(PREFIX)/bin
PROFILE ?= profiles/lenovo-82wq-legion-pro-7-16irx8h.json
PRE_COMMIT ?= pre-commit
PYTHON ?= python
LEGIONCTL = $(PYTHON) src/legionctl/legionctl --profile $(PROFILE)

.PHONY: help
help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-10s %s\n", $$1, $$2}'

.PHONY: tools
tools: ## install dev tooling (pre-commit; hooks fetch ruff, mypy, shellcheck, shfmt, actionlint)
	@command -v uv >/dev/null || { echo "uv is required: https://docs.astral.sh/uv/getting-started/installation/"; exit 1; }
	uv tool install --upgrade pre-commit
	$(PRE_COMMIT) install-hooks

.PHONY: hooks
hooks: tools ## install the git pre-commit hook
	$(PRE_COMMIT) install

.PHONY: fmt
fmt: ## format python, shell and rust in place
	cargo fmt --all
	-$(PRE_COMMIT) run ruff-check --all-files
	-$(PRE_COMMIT) run ruff-format --all-files
	-$(PRE_COMMIT) run shfmt --all-files

.PHONY: lint
lint: ## run every pre-commit hook on all files
	$(PRE_COMMIT) run --all-files --show-diff-on-failure

.PHONY: rust-lint
rust-lint: ## rustfmt check and clippy with warnings as errors
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: build
build: ## build the legionctl client binary (named legion until the switch) (run as your user, before install)
	cargo build --release --locked

.PHONY: validate
validate: ## check profile structure and ac/battery consistency (no hardware access)
	$(LEGIONCTL) validate

.PHONY: test
test: ## unit tests (no hardware access)
	$(PYTHON) -m unittest discover -s tests -v
	cargo test --workspace --locked

.PHONY: package
package: ## build and install the Arch package (pacman tracks every file)
	cd packaging/arch && makepkg -sfi

.PHONY: lint-pkg
lint-pkg: ## namcap the PKGBUILD (Arch only)
	namcap packaging/arch/PKGBUILD

.PHONY: ci
ci: lint rust-lint validate test ## what CI runs

.PHONY: install
install: ## install legionctl, profile, units, udev and tuning (needs root)
	install -Dm755 src/legionctl/legionctl "$(DESTDIR)$(BINDIR)/legionctl"
	@test -x target/release/legion || { echo "target/release/legion missing: run 'make build' as your user first"; exit 1; }
	install -Dm755 target/release/legion "$(DESTDIR)$(BINDIR)/legion"
	install -d "$(DESTDIR)$(DATADIR)" && rm -rf "$(DESTDIR)$(DATADIR)/agent-plugin" && cp -r integrations/agent-plugin "$(DESTDIR)$(DATADIR)/"
	install -Dm755 src/legion-refresh-toggle/legion-refresh-toggle "$(DESTDIR)$(BINDIR)/legion-refresh-toggle"
	install -Dm755 src/legion-osd/legion-osd "$(DESTDIR)$(BINDIR)/legion-osd"
	install -Dm644 systemd/user/legion-osd.service "$(DESTDIR)$(PREFIX)/lib/systemd/user/legion-osd.service"
	install -Dm644 profiles/lenovo-82wq-legion-pro-7-16irx8h.json "$(DESTDIR)$(DATADIR)/profiles/lenovo-82wq-legion-pro-7-16irx8h.json"
	install -Dm644 systemd/legion-ac.target "$(DESTDIR)$(UNITDIR)/legion-ac.target"
	install -Dm644 systemd/legion-battery.target "$(DESTDIR)$(UNITDIR)/legion-battery.target"
	install -Dm644 systemd/legion-apply.service "$(DESTDIR)$(UNITDIR)/legion-apply.service"
	install -Dm644 systemd/legion-apply-ac.service "$(DESTDIR)$(UNITDIR)/legion-apply-ac.service"
	install -Dm644 systemd/legion-apply-battery.service "$(DESTDIR)$(UNITDIR)/legion-apply-battery.service"
	install -Dm644 systemd/legion-reassert-ac.service "$(DESTDIR)$(UNITDIR)/legion-reassert-ac.service"
	install -Dm644 systemd/legion-reassert-ac.timer "$(DESTDIR)$(UNITDIR)/legion-reassert-ac.timer"
	install -Dm644 systemd/legion-follow.service "$(DESTDIR)$(UNITDIR)/legion-follow.service"
	install -Dm755 systemd/system-sleep/legion "$(DESTDIR)$(PREFIX)/lib/systemd/system-sleep/legion"
	install -Dm644 udev/99-legionctl.rules "$(DESTDIR)$(UDEVDIR)/99-legionctl.rules"
	install -Dm644 hwdb/90-legion-keys.hwdb "$(DESTDIR)$(SYSCONFDIR)/udev/hwdb.d/90-legion-keys.hwdb"
	install -Dm644 systemd/power-profiles-daemon.service.d/10-legion.conf "$(DESTDIR)$(UNITDIR)/power-profiles-daemon.service.d/10-legion.conf"
	install -Dm644 tuning/scx_loader.toml "$(DESTDIR)$(SYSCONFDIR)/scx_loader.toml"

.PHONY: check
check: validate ## compile + live status on this machine
	$(PYTHON) -m py_compile src/legionctl/legionctl
	$(LEGIONCTL) status
