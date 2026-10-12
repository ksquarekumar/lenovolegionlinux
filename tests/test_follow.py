"""Fn+Q follow state machine: own writes are ignored, user changes become an override, apply clears it."""

import importlib.machinery
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("legionctl", str(ROOT / "src/legionctl/legionctl"))
spec = importlib.util.spec_from_loader("legionctl", loader)
assert spec is not None
legionctl = importlib.util.module_from_spec(spec)
loader.exec_module(legionctl)


class FollowTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.platform = self.tmp / "platform_profile"
        profile = json.loads((ROOT / "profiles/lenovo-82wq-legion-pro-7-16irx8h.json").read_text())
        profile["paths"]["platform_profile"] = str(self.platform)
        self.profile_path = self.tmp / "profile.json"
        self.profile_path.write_text(json.dumps(profile))
        state = self.tmp / "state"
        legionctl.STATE_DIR = state
        legionctl.WRITTEN_FILE = state / "firmware-written"
        legionctl.OVERRIDE_FILE = state / "firmware-override"
        self.calls = []
        legionctl.set_power_profile = lambda runner, wanted: self.calls.append(("ppd", wanted))
        legionctl.write_cpufreq = lambda runner, name, value: self.calls.append((name, value))

    def follow(self, firmware_mode):
        self.platform.write_text(f"{firmware_mode}\n")
        return legionctl.cmd_follow(SimpleNamespace(profile=str(self.profile_path), dry_run=False, quiet=True))

    def test_own_write_is_ignored(self):
        legionctl.save_state(legionctl.Runner(quiet=True), legionctl.WRITTEN_FILE, "max-power")
        self.follow("max-power")
        self.assertIsNone(legionctl.firmware_override())
        self.assertEqual(self.calls, [])

    def test_fn_q_change_becomes_override_and_drives_ppd_and_governor(self):
        legionctl.save_state(legionctl.Runner(quiet=True), legionctl.WRITTEN_FILE, "max-power")
        self.follow("balanced")
        self.assertEqual(legionctl.firmware_override(), "balanced")
        self.assertEqual(self.calls, [("ppd", "balanced"), ("scaling_governor", "powersave")])

    def test_repeat_event_for_active_override_is_ignored(self):
        legionctl.save_state(legionctl.Runner(quiet=True), legionctl.WRITTEN_FILE, "max-power")
        self.follow("balanced")
        self.calls.clear()
        self.follow("balanced")
        self.assertEqual(self.calls, [])

    def test_apply_clears_override(self):
        legionctl.save_state(legionctl.Runner(quiet=True), legionctl.OVERRIDE_FILE, "low-power")
        legionctl.clear_firmware_override(legionctl.Runner(quiet=True))
        self.assertIsNone(legionctl.firmware_override())


if __name__ == "__main__":
    unittest.main()
