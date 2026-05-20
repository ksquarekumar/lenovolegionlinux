PREFIX ?= /usr
SYSCONFDIR ?= /etc
UNITDIR ?= $(PREFIX)/lib/systemd/system
UDEVDIR ?= $(SYSCONFDIR)/udev/rules.d
DATADIR ?= $(PREFIX)/share/legion-control
BINDIR ?= $(PREFIX)/bin

.PHONY: install
install:
	install -Dm755 src/legionctl/legionctl "$(DESTDIR)$(BINDIR)/legionctl"
	install -Dm644 profiles/lenovo-82wq-legion-pro-7-16irx8h.json "$(DESTDIR)$(DATADIR)/profiles/lenovo-82wq-legion-pro-7-16irx8h.json"
	install -Dm644 systemd/legion-ac.target "$(DESTDIR)$(UNITDIR)/legion-ac.target"
	install -Dm644 systemd/legion-battery.target "$(DESTDIR)$(UNITDIR)/legion-battery.target"
	install -Dm644 systemd/legion-apply.service "$(DESTDIR)$(UNITDIR)/legion-apply.service"
	install -Dm644 systemd/legion-apply-ac.service "$(DESTDIR)$(UNITDIR)/legion-apply-ac.service"
	install -Dm644 systemd/legion-apply-battery.service "$(DESTDIR)$(UNITDIR)/legion-apply-battery.service"
	install -Dm644 systemd/legion-reassert-ac.service "$(DESTDIR)$(UNITDIR)/legion-reassert-ac.service"
	install -Dm644 systemd/legion-reassert-ac.timer "$(DESTDIR)$(UNITDIR)/legion-reassert-ac.timer"
	install -Dm644 udev/99-legion-control.rules "$(DESTDIR)$(UDEVDIR)/99-legion-control.rules"

.PHONY: check
check:
	python -m py_compile src/legionctl/legionctl
	python src/legionctl/legionctl --profile profiles/lenovo-82wq-legion-pro-7-16irx8h.json status
