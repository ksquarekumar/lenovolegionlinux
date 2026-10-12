"""Firmware fan RPM via acpi_call: parse every reply shape /proc/acpi/call produces."""

import importlib.machinery
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("legionctl_fan", str(ROOT / "src/legionctl/legionctl"))
spec = importlib.util.spec_from_loader("legionctl_fan", loader)
assert spec is not None
legionctl = importlib.util.module_from_spec(spec)
loader.exec_module(legionctl)


class ParseAcpiCallTest(unittest.TestCase):
    def test_integer_reply(self):
        self.assertEqual(legionctl.parse_acpi_call("0x960"), 2400)

    def test_stopped_fan(self):
        self.assertEqual(legionctl.parse_acpi_call("0x0\n"), 0)

    def test_buffer_reply_is_little_endian(self):
        self.assertEqual(legionctl.parse_acpi_call("{0x60, 0x09, 0x00, 0x00}"), 2400)

    def test_trailing_nul(self):
        self.assertEqual(legionctl.parse_acpi_call("0x1194\x00"), 4500)

    def test_error_is_unknown(self):
        self.assertIsNone(legionctl.parse_acpi_call("Error: AE_NOT_FOUND"))


if __name__ == "__main__":
    unittest.main()
