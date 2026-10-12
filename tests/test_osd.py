"""OSD wording: an Fn+Q override is labelled as such, otherwise the power-source default is named."""

import importlib.machinery
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("osd", str(ROOT / "src/legion-osd/legion-osd"))
spec = importlib.util.spec_from_loader("osd", loader)
assert spec is not None
osd = importlib.util.module_from_spec(spec)
loader.exec_module(osd)


class DescribeModeTest(unittest.TestCase):
    def test_fn_q_override(self):
        summary, body, _ = osd.describe_mode("balanced", "balanced", "1")
        self.assertEqual((summary, body), ("Thermal mode: Balanced", "Fn+Q choice until unplug or reboot"))

    def test_ac_default(self):
        self.assertEqual(osd.describe_mode("max-power", None, "1")[:2], ("Thermal mode: Max power", "AC default"))

    def test_battery_default(self):
        self.assertEqual(osd.describe_mode("balanced", None, "0")[1], "battery default")

    def test_unknown_mode_is_shown_raw(self):
        self.assertEqual(osd.describe_mode("extreme", None, "1")[0], "Thermal mode: extreme")


class FnRPressesTest(unittest.TestCase):
    def event(self, sec, code, value, etype=1):
        return osd.INPUT_EVENT.pack(sec, 0, etype, code, value)

    def test_key_down_of_either_code_counts(self):
        data = self.event(1, 562, 1) + self.event(1, 562, 0) + self.event(2, 188, 1)
        self.assertEqual(osd.fn_r_presses(data), [1.0, 2.0])

    def test_other_keys_scans_and_releases_are_ignored(self):
        data = self.event(1, 272, 30, etype=4) + self.event(1, 30, 1) + self.event(1, 562, 0)
        self.assertEqual(osd.fn_r_presses(data), [])


if __name__ == "__main__":
    unittest.main()
