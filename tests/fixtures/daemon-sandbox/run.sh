#!/bin/bash
# The daemon's sandbox test in the guest (crates/legionctl/tests/e2e.rs): legionctl-system runs under the package's
# own unit (ProtectSystem=strict and its ReadWritePaths), and root calls the methods that write /etc and the ESP over
# the system bus, through polkit. Stand-ins: greetd, cage and GDM (units that do nothing), the signers and efibootmgr
# (their state in /dev/shm, which the sandbox and the transient units can write), a kernel and the MOK key. The ESP is
# a vfat partition of a loop device at /boot/efi: it is mounted after the daemon started, then it is an automount
# (x-systemd.automount) that the daemon's first open mounts. Each check writes "ok <name>" or "FAIL <name>: <why>" to the results
# disk (serial lwmiresults); the test reads it after power-off.
set -u
here=$(dirname "$0")
results=/var/lib/sandbox-results
: >"$results"

ok() { echo "ok $1" >>"$results"; }
fail() { echo "FAIL $1: $2" >>"$results"; }
check() { # check <name> <command...>: ok when the command succeeds
  local name=$1 out
  shift
  if out=$("$@" 2>&1); then ok "$name"; else fail "$name" "$out"; fi
}
finish() {
  {
    echo "daemon log:"
    journalctl -b -u legionctl-system -u 'legionctl-step-*' --no-pager -o cat | tail -n 40
    echo "done"
  } >>"$results"
  dd if="$results" of=/dev/disk/by-id/virtio-lwmiresults conv=notrunc status=none
  sync
}
trap finish EXIT
# the getty's hangup of ttyS0 ends this script
systemctl mask --now serial-getty@ttyS0.service

# the package's files, where make install puts them
install -Dm755 "$here/legionctl-system" /usr/lib/legionctl/legionctl-system
install -Dm644 "$here/legionctl-system.service" /usr/lib/systemd/system/legionctl-system.service
install -Dm644 "$here/org.legionctl.System1.conf" /usr/share/dbus-1/system.d/org.legionctl.System1.conf
install -Dm644 "$here/org.legionctl.policy" /usr/share/polkit-1/actions/org.legionctl.policy
install -Dm644 -t /usr/share/legionctl/profiles "$here"/profiles/*.json
mkdir -p /usr/share/legionctl/configsets && cp -r "$here/configsets/." /usr/share/legionctl/configsets/
install -dm755 /etc/greetd
# polkit's own folder of rules, as on Arch
install -dm750 /etc/polkit-1/rules.d
systemctl reload dbus

# greetd (with the user greeter that its package makes), cage and the greeter are there; GDM is the display manager
for f in /usr/bin/greetd /usr/bin/cage /usr/lib/legionctl/legionctl-greeter; do install -Dm755 /bin/true "$f"; done
id greeter >/dev/null 2>&1 || useradd --system --no-create-home --home-dir / greeter
for unit in gdm greetd; do
  printf '[Service]\nExecStart=/bin/true\n\n[Install]\nAlias=display-manager.service\n' >"/usr/lib/systemd/system/$unit.service"
done
systemctl daemon-reload
systemctl enable gdm.service

# the boot chain: an ESP with shim and MokManager, the MOK key, systemd-boot and one kernel with its initramfs
truncate -s 64M /var/tmp/esp.img
echo 'type=uefi' | sfdisk -q --label gpt /var/tmp/esp.img
loop=$(losetup -fP --show /var/tmp/esp.img)
# systemd stops a mount whose device udev did not announce yet: it unmounted /boot/efi in CI run 38754218394
udevadm settle
mkfs.vfat "${loop}p1" >/dev/null
# the image's own ESP goes: the daemon starts with no ESP
umount /boot/efi
mount "${loop}p1" /boot/efi
mkdir -p /boot/efi/EFI/BOOT /etc/refind.d/keys /usr/lib/systemd/boot/efi /usr/lib/modules/0.0.0-test
echo shim >/boot/efi/EFI/BOOT/bootx64.efi
echo mokmanager >/boot/efi/EFI/BOOT/mmx64.efi
umount /boot/efi
echo key >/etc/refind.d/keys/refind_local.key
echo cert >/etc/refind.d/keys/refind_local.crt
[ -e /usr/lib/systemd/boot/efi/systemd-bootx64.efi ] || echo systemd-boot >/usr/lib/systemd/boot/efi/systemd-bootx64.efi
echo kernel >/boot/vmlinuz-linux-test
echo initramfs >/boot/initramfs-linux-test.img
echo kernel >/usr/lib/modules/0.0.0-test/vmlinuz
echo linux-test >/usr/lib/modules/0.0.0-test/pkgbase
# the signers write their arguments and their unit into the output; efibootmgr keeps the firmware's entries in a file
cat >/usr/local/bin/signer <<'EOF'
#!/bin/sh
out=; prev=; for a in "$@"; do [ "$prev" = --output ] && out=$a; prev=$a; done
echo "$(basename "$0") $* unit=$(cut -d: -f3 /proc/self/cgroup)" >"$out"
EOF
chmod 755 /usr/local/bin/signer
ln -s signer /usr/local/bin/ukify
ln -s signer /usr/local/bin/sbsign
cat >/usr/local/bin/sbverify <<'EOF'
#!/bin/sh
grep -q -e sbsign -e ukify "$3"
EOF
chmod 755 /usr/local/bin/sbverify
cat >/usr/local/bin/efibootmgr <<'EOF'
#!/usr/bin/python3
import json, os, sys
path = "/dev/shm/efi.json"
s = json.load(open(path)) if os.path.exists(path) else {"entries": {"0003": "rEFInd"}, "order": ["0003"], "next": None}
a = sys.argv[1:]
if "--create-only" in a:
    s["entries"]["0007"] = a[a.index("--label") + 1]
elif "--bootnext" in a:
    s["next"] = a[a.index("--bootnext") + 1]
json.dump(s, open(path, "w"))
print("BootCurrent: 0003")
if s["next"]: print("BootNext: " + s["next"])
print("BootOrder: " + ",".join(s["order"]))
for k, v in sorted(s["entries"].items()): print("Boot" + k + "* " + v)
EOF
chmod 755 /usr/local/bin/efibootmgr

systemctl start legionctl-system.service
check unit-active systemctl is-active legionctl-system.service
pid=$(systemctl show -P MainPID legionctl-system.service)
check esp-after-start nsenter -t "$pid" -m sh -c '! grep -q " vfat " /proc/self/mounts'
mount "${loop}p1" /boot/efi
if nsenter -t "$pid" -m touch /usr/lib/legionctl/written 2>/dev/null; then
  fail usr-read-only "the daemon can write /usr"
else
  ok usr-read-only
fi

call() { busctl --timeout=120 call org.legionctl.System1 /org/legionctl/System1 org.legionctl.System1 "$@"; }
# the MOK key folder is root's alone, as on Arch (todo-128): a user's dry run is the daemon's plan, through the user's
# own operator, without polkit (the user has no session), and it changes nothing
chmod 700 /etc/refind.d/keys
useradd -m tester
install -Dm755 "$here/legionctl-operator" /usr/lib/legionctl/legionctl-operator
install -Dm755 "$here/legionctl" /usr/bin/legionctl
rt=/run/tester
install -d -o tester -m700 "$rt" "$rt/legionctl"
# systemd-socket-activate gives the operator only the variables it names
runuser -u tester -- systemd-socket-activate -l "$rt/legionctl/api.sock" -E HOME=/home/tester -E XDG_RUNTIME_DIR="$rt" \
  /usr/lib/legionctl/legionctl-operator &
for _ in $(seq 50); do
  [ -S "$rt/legionctl/api.sock" ] && break
  sleep 0.1
done
check keys-root-only sh -c '! runuser -u tester -- test -e /etc/refind.d/keys/refind_local.key'
dry_run() {
  local out
  out=$(runuser -u tester -- env XDG_RUNTIME_DIR="$rt" legionctl boot install --dry-run 2>&1) || {
    echo "$out"
    return 1
  }
  grep -q -- "--label legionctl" <<<"$out" && grep -q "Restart once: the next start uses legionctl" <<<"$out" && [ ! -e /boot/efi/EFI/legionctl ] || {
    echo "$out"
    return 1
  }
}
check boot-dry-run-user dry_run
check greeter-install call Greeter s install
check greeter-config grep -q '^# legionctl greeter' /etc/greetd/config.toml
check greeter-display-manager sh -c 'readlink /etc/systemd/system/display-manager.service | grep -q greetd.service'
check boot-install call Boot s install
check boot-loader grep -q 'unit=/system.slice/legionctl-step-' /boot/efi/EFI/legionctl/grubx64.efi
check boot-image grep -q 'unit=/system.slice/legionctl-step-' /boot/efi/EFI/Linux/legionctl-linux-test.efi
check boot-line test -s /etc/legionctl/kernel/linux-test.cmdline
check boot-loader-conf grep -q 'default legionctl-linux-test.efi' /boot/efi/loader/loader.conf
check boot-entry python3 -c 'import json; s = json.load(open("/dev/shm/efi.json")); assert s["entries"]["0007"] == "legionctl" and s["next"] == "0007", s'
# the ESP as an automount: the daemon's rebuild must mount it to make the image again
rm /boot/efi/EFI/Linux/legionctl-linux-test.efi
umount /boot/efi
sed -i '\| /boot/efi |d' /etc/fstab
echo "${loop}p1 /boot/efi vfat noauto,x-systemd.automount 0 0" >>/etc/fstab
systemctl daemon-reload
systemctl start boot-efi.automount
check esp-automount-idle sh -c '! grep -q " /boot/efi vfat " /proc/self/mounts'
check boot-rebuild-automount call Boot s rebuild
check boot-image-automount grep -q 'unit=/system.slice/legionctl-step-' /boot/efi/EFI/Linux/legionctl-linux-test.efi
check autostart-rule call SetWindowsAutostart b true
check autostart-rule-file grep -q 'subject.user == "root"' /etc/polkit-1/rules.d/49-legionctl-windows-vm-root.rules
call SetPrimaryGpu sb intel false >/dev/null
check primary-gpu-rule test -s /etc/udev/rules.d/61-legionctl-mutter-primary.rules
