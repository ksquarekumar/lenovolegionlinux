"""Fn+R mode choice: highest refresh at the current size and VRR state, else the mode closest to 60 Hz."""

import importlib.machinery
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("refresh", str(ROOT / "src/legion-refresh-toggle/legion-refresh-toggle"))
spec = importlib.util.spec_from_loader("refresh", loader)
assert spec is not None
refresh = importlib.util.module_from_spec(spec)
loader.exec_module(refresh)


def mode(w, h, hz, current=False, vrr=False):
    props = {"is-current": current, "refresh-rate-mode": "variable" if vrr else "fixed"}
    return (f"{w}x{h}@{hz}{'+vrr' if vrr else ''}", w, h, hz, 1.0, [1.0], props)


PANEL = [
    mode(2560, 1600, 240.003),
    mode(2560, 1600, 240.003, vrr=True),
    mode(2560, 1600, 60.002),
    mode(2560, 1600, 59.987),
    mode(1920, 1200, 240.0),
]


class PickTargetTest(unittest.TestCase):
    def test_high_goes_to_closest_60(self):
        self.assertEqual(refresh.pick_target(PANEL, PANEL[0])[0], "2560x1600@60.002")

    def test_low_goes_to_highest_fixed_mode_not_vrr(self):
        self.assertEqual(refresh.pick_target(PANEL, PANEL[2])[0], "2560x1600@240.003")

    def test_vrr_stays_vrr_both_ways(self):
        panel = [*PANEL, mode(2560, 1600, 60.002, vrr=True)]
        low = refresh.pick_target(panel, panel[1])
        self.assertEqual(low[0], "2560x1600@60.002+vrr")
        self.assertEqual(refresh.pick_target(panel, low)[0], "2560x1600@240.003+vrr")

    def test_stays_at_current_resolution(self):
        self.assertEqual(refresh.pick_target(PANEL, PANEL[3])[1:3], (2560, 1600))


class DebounceTest(unittest.TestCase):
    def test_burst_from_one_press_toggles_once(self):
        import tempfile

        stamp = Path(tempfile.mkdtemp()) / "last"
        presses = [100.0, 100.1, 100.25, 100.9, 101.2]  # one Fn+R press as captured
        self.assertEqual([refresh.debounced(stamp, t) for t in presses], [False, True, True, True, True])

    def test_next_press_after_window_toggles_again(self):
        import tempfile

        stamp = Path(tempfile.mkdtemp()) / "last"
        refresh.debounced(stamp, 100.0)
        self.assertFalse(refresh.debounced(stamp, 102.0))


if __name__ == "__main__":
    unittest.main()
