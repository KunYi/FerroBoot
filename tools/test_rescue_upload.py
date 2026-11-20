"""Host-side checks for rescue_upload.py (no serial port needed): python3 -m unittest tools/test_rescue_upload.py"""
import os
import sys
import types
import unittest

# rescue_upload imports pyserial at module level; the helpers tested here do not use it.
sys.modules.setdefault("serial", types.ModuleType("serial"))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rescue_upload  # noqa: E402

# First 32 bytes of `xtask sign --major 4 --minor 2 --rev 3 --build 7` on a 4096-byte input,
# taken from the Rust tool's own output (not computed here).
XTASK_HEADER_4_2_3_B7 = bytes.fromhex(
    "3db8f3960000002400040c000010000020000000040203000700000000000000"
)


class HeaderVersion(unittest.TestCase):
    def test_matches_what_xtask_wrote(self):
        self.assertEqual(rescue_upload.header_version(XTASK_HEADER_4_2_3_B7), "4.2.3+7")

    def test_default_xtask_version_is_1_0_0_plus_1(self):
        hdr = bytearray(XTASK_HEADER_4_2_3_B7)
        hdr[20:28] = bytes([1, 0, 0, 0, 1, 0, 0, 0])
        self.assertEqual(rescue_upload.header_version(bytes(hdr)), "1.0.0+1")

    def test_fields_are_not_mixed_up(self):
        hdr = bytearray(XTASK_HEADER_4_2_3_B7)
        hdr[20:28] = bytes([9, 8, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12])
        self.assertEqual(rescue_upload.header_version(bytes(hdr)), "9.8.4660+305419896")


if __name__ == "__main__":
    unittest.main()
