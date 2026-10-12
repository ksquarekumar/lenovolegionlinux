"""Platform helpers: module autoload/DKMS parsing, GPU wiring, primary-GPU rule, throttle bits, core classes."""

import importlib.machinery
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("legionctl_platform", str(ROOT / "src/legionctl/legionctl"))
spec = importlib.util.spec_from_loader("legionctl_platform", loader)
assert spec is not None
legionctl = importlib.util.module_from_spec(spec)
loader.exec_module(legionctl)


def fake_drm(root, cards):
    """cards: {card: (driver, {connector: status})}"""
    drm = root / "drm"
    for card, (driver, connectors) in cards.items():
        driver_dir = root / "drivers" / driver
        driver_dir.mkdir(parents=True, exist_ok=True)
        (drm / card / "device").mkdir(parents=True)
        os.symlink(driver_dir, drm / card / "device" / "driver")
        for name, status in connectors.items():
            (drm / f"{card}-{name}").mkdir()
            (drm / f"{card}-{name}" / "status").write_text(status + "\n")
    return drm


class ModulesTest(unittest.TestCase):
    def test_parse_modules_load_skips_comments_and_blanks(self):
        self.assertEqual(legionctl.parse_modules_load("# x\nacpi_call\n\n; y\nideapad_laptop"), ["acpi_call", "ideapad_laptop"])

    def test_parse_dkms_status(self):
        text = "acpi_call/1.2.2, 7.2.9-2-cachyos, x86_64: installed\nnvidia/580.178.04, 7.2.9-2-cachyos-bore-lto, x86_64: built\n"
        self.assertEqual(
            legionctl.parse_dkms_status(text),
            [("acpi_call/1.2.2", "7.2.9-2-cachyos", "installed"), ("nvidia/580.178.04", "7.2.9-2-cachyos-bore-lto", "built")],
        )

    def test_profile_declares_the_modules_this_machine_needs(self):
        import json

        profile = json.loads((ROOT / "profiles/lenovo-82wq-legion-pro-7-16irx8h.json").read_text())
        self.assertIn("acpi_call", profile["kernel_modules"]["required"])
        self.assertIn("thinkpad_acpi", profile["kernel_modules"]["unwanted_autoload"])


class GpuTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())

    def test_hybrid_when_panel_is_on_intel(self):
        drm = fake_drm(
            self.root,
            {"card0": ("nvidia", {"DP-3": "connected", "eDP-2": "disconnected"}), "card1": ("i915", {"eDP-1": "connected"})},
        )
        outputs = legionctl.gpu_outputs(drm)
        self.assertEqual(legionctl.gpu_mode(outputs), "hybrid")
        self.assertIn(("DP-3", "card0", "nvidia", True), outputs)

    def test_discrete_when_panel_is_on_nvidia(self):
        drm = fake_drm(self.root, {"card0": ("nvidia", {"eDP-2": "connected"}), "card1": ("i915", {"eDP-1": "disconnected"})})
        self.assertEqual(legionctl.gpu_mode(legionctl.gpu_outputs(drm)), "discrete")

    def test_primary_rule_round_trip(self):
        legionctl.PRIMARY_GPU_RULE = self.root / "61.rules"
        self.assertEqual(legionctl.preferred_primary_gpu(), "auto")
        legionctl.PRIMARY_GPU_RULE.write_text(legionctl.primary_gpu_rule("nvidia"))
        self.assertEqual(legionctl.preferred_primary_gpu(), "nvidia")
        self.assertIn("mutter-device-preferred-primary", legionctl.primary_gpu_rule("intel"))


class CpuTest(unittest.TestCase):
    def test_throttle_reason_bits(self):
        self.assertEqual(legionctl.throttle_reasons(0b1000_0000_1010), ["thermal", "PROCHOT", "power limit"])
        self.assertEqual(legionctl.throttle_reasons(0), [])

    def test_core_classes(self):
        devices = Path(tempfile.mkdtemp())
        for name, cpus in (("cpu_core", "0-15"), ("cpu_atom", "16-31")):
            (devices / name).mkdir()
            (devices / name / "cpus").write_text(cpus + "\n")
        self.assertEqual(legionctl.cpu_class_cpus("p", devices), "0-15")
        self.assertEqual(legionctl.cpu_class_cpus("e", devices), "16-31")


if __name__ == "__main__":
    unittest.main()
