"""Host-side checks for rescue_upload.py (no serial port needed): python3 -m unittest tools/test_rescue_upload.py"""
import os
import random
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

# The same literal is asserted by crates/boot-rescue (H750_INFO) against the Rust code that
# writes it, so this parser and the bootloader are pinned to one string.
H750_INFO = ("INFO proto=2 board=h750-netlite bl=1 flash=4194304 align=1 chunk=512 "
             "slot0=655360 slot1=655360 window=524288")


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


class ParseInfo(unittest.TestCase):
    def test_the_h750_line(self):
        info = rescue_upload.parse_info(H750_INFO)
        self.assertEqual(info, {
            "proto": 2, "board": "h750-netlite", "bl": 1, "flash": 4194304, "align": 1,
            "chunk": 512, "slot0": 655360, "slot1": 655360, "window": 524288,
        })
        rescue_upload.check_info(info)  # usable

    def test_an_old_bootloader_is_recognised_as_such(self):
        with self.assertRaisesRegex(ValueError, "not an INFO line"):
            rescue_upload.parse_info("[RESCUE] BL v1.0, STM32H750, NOR BY25Q32ES")
        with self.assertRaisesRegex(ValueError, "not an INFO line"):
            rescue_upload.parse_info("")

    def test_unusable_lines_are_refused_with_a_reason(self):
        bad = {
            "missing field": H750_INFO.replace(" window=524288", ""),
            "empty value": H750_INFO.replace("align=1", "align="),
            "no equals": H750_INFO.replace("align=1", "align"),
            "not a number": H750_INFO.replace("flash=4194304", "flash=4MiB"),
            "negative": H750_INFO.replace("bl=1", "bl=-1"),
            "hex": H750_INFO.replace("chunk=512", "chunk=0x200"),
            "duplicate": H750_INFO + " align=4",
            "double space": H750_INFO.replace("bl=1 ", "bl=1  "),
            "unicode digit": H750_INFO.replace("bl=1", "bl=\u00b2"),
            # int() accepts these as numbers (this is 3); the protocol does not
            "arabic-indic digit": H750_INFO.replace("bl=1", "bl=\u0663"),
            "empty board name": H750_INFO.replace("board=h750-netlite", "board="),
            "empty unknown field": H750_INFO + " x=",
        }
        for what, line in bad.items():
            with self.assertRaises(ValueError, msg=what):
                rescue_upload.parse_info(line)

    def test_unknown_keys_are_ignored_so_the_bootloader_can_grow_the_line(self):
        info = rescue_upload.parse_info(H750_INFO + " erase=64 board2=x")
        self.assertEqual(info["slot0"], 655360)
        self.assertEqual(info["board"], "h750-netlite")
        # ...but a known number that is not one is still refused
        with self.assertRaises(ValueError):
            rescue_upload.parse_info(H750_INFO.replace("slot0=655360", "slot0=x"))

    def test_another_protocol_version_is_refused(self):
        for proto in (1, 3, 0):
            info = rescue_upload.parse_info(H750_INFO.replace("proto=2", f"proto={proto}"))
            with self.assertRaisesRegex(ValueError, "protocol"):
                rescue_upload.check_info(info)

    def test_a_chunk_that_is_not_whole_words_is_refused(self):
        for chunk, align in ((512, 3), (2, 4), (0, 1)):
            info = rescue_upload.parse_info(
                H750_INFO.replace("chunk=512", f"chunk={chunk}").replace("align=1", f"align={align}"))
            with self.assertRaises(ValueError, msg=f"{chunk}/{align}"):
                rescue_upload.check_info(info)


class SizeLimits(unittest.TestCase):
    SLOT, WINDOW = 640 * 1024, 512 * 1024

    def test_window_is_the_practical_limit_and_slot_the_hard_one(self):
        p = rescue_upload.size_problem
        self.assertIsNone(p(512 * 1024, self.SLOT, self.WINDOW))
        self.assertEqual(p(512 * 1024 + 1, self.SLOT, self.WINDOW)[0], "warning")
        self.assertEqual(p(640 * 1024, self.SLOT, self.WINDOW)[0], "warning")
        self.assertEqual(p(640 * 1024 + 1, self.SLOT, self.WINDOW)[0], "error")

    def test_no_window_means_only_the_slot_limits(self):
        p = rescue_upload.size_problem
        self.assertIsNone(p(self.SLOT, self.SLOT, 0))
        self.assertEqual(p(self.SLOT + 1, self.SLOT, 0)[0], "error")

    def test_the_padded_upload_must_fit_the_slot_too(self):
        p = rescue_upload.size_problem
        # 4-byte words, a slot of 100 bytes: 99 bytes pad to 100 and fit, 101 do not
        self.assertIsNone(p(99, 100, 0, align=4))
        self.assertEqual(p(101, 100, 0, align=4)[0], "error")
        # 97 pads to 100: fits; but a slot of 98 does not take it
        self.assertIsNone(p(97, 100, 0, align=4))
        self.assertEqual(p(97, 98, 0, align=4)[0], "error")


class PlanChunks(unittest.TestCase):
    def test_byte_programming_sends_the_image_as_is(self):
        img = bytes(range(256)) * 5 + b"xyz"  # 1283 bytes
        plan = list(rescue_upload.plan_chunks(img, 512, 1))
        self.assertEqual([(o, len(d)) for o, d in plan], [(0, 512), (512, 512), (1024, 259)])
        self.assertEqual(b"".join(d for _, d in plan), img)

    def test_the_last_piece_is_padded_with_ff_to_a_whole_word(self):
        img = bytes([0x11]) * 1301  # 1301 = 3 * 4 + 1 + 1288 ... last piece is 277 bytes
        plan = list(rescue_upload.plan_chunks(img, 512, 4))
        self.assertEqual([(o, len(d)) for o, d in plan], [(0, 512), (512, 512), (1024, 280)])
        self.assertEqual(plan[-1][1], bytes([0x11]) * 277 + b"\xff" * 3)

    def test_an_image_that_is_already_whole_words_is_not_padded(self):
        img = bytes([0x22]) * 1024
        self.assertEqual(b"".join(d for _, d in rescue_upload.plan_chunks(img, 512, 4)), img)

    def test_every_piece_is_valid_for_any_size_and_word(self):
        rng = random.Random(1234)
        for _ in range(300):
            align = rng.choice([1, 2, 4, 8, 16, 32])
            chunk = rng.choice([a for a in (align, 2 * align, 64, 128, 512) if a % align == 0])
            n = rng.randrange(1, 3000)
            img = bytes(rng.randrange(256) for _ in range(n))
            plan = list(rescue_upload.plan_chunks(img, chunk, align))
            at = 0
            for off, data in plan:
                self.assertEqual(off, at)
                self.assertTrue(0 < len(data) <= chunk and len(data) % align == 0)
                at += len(data)
            joined = b"".join(d for _, d in plan)
            self.assertEqual(joined[:n], img)
            self.assertEqual(joined[n:], b"\xff" * (len(joined) - n))
            self.assertLess(len(joined) - n, align)  # never more padding than one word

    def test_an_empty_image_sends_nothing_and_a_bad_chunk_is_refused(self):
        self.assertEqual(list(rescue_upload.plan_chunks(b"", 512, 4)), [])
        for chunk, align in ((512, 3), (2, 4), (0, 1), (512, 0)):
            with self.assertRaises(ValueError, msg=f"{chunk}/{align}"):
                list(rescue_upload.plan_chunks(b"abc", chunk, align))


class FakeLink:
    """A serial port that delivers bytes at given (absolute) times on a fake clock."""

    def __init__(self, arrivals):
        self.arrivals = list(arrivals)  # (time, byte), in order
        self.clock = 0.0

    def now(self):
        return self.clock

    def read(self, n):
        assert n == 1
        step = 0.05  # like pyserial's read timeout
        if self.arrivals and self.arrivals[0][0] <= self.clock + step:
            t, b = self.arrivals.pop(0)
            self.clock = max(self.clock, t)
            return b
        self.clock += step
        return b""


def at(times_and_text):
    out = []
    for t, text in times_and_text:
        out += [(t, bytes([c])) for c in text]
    return out


class EraseReply(unittest.TestCase):
    def test_dots_then_ok(self):
        link = FakeLink(at([(1.0 + i, b".") for i in range(10)] + [(11.0, b"OK\r\n")]))
        seen = []
        steps, line = rescue_upload.read_erase_reply(link, 5.0, on_step=seen.append, now=link.now)
        self.assertEqual((steps, line), (10, "OK"))
        self.assertEqual(seen, list(range(1, 11)))

    def test_a_failure_reply_is_returned_not_raised(self):
        link = FakeLink(at([(1.0, b"."), (2.0, b"."), (3.0, b"ERR\r\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (2, "ERR"))

    def test_an_error_reply_without_any_step(self):
        link = FakeLink(at([(0.2, b"ERR_ALIGN\r\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (0, "ERR_ALIGN"))

    def test_the_time_limit_is_per_step_not_for_the_whole_erase(self):
        # ten steps, 9 s apart: 90 s in all, each gap under the 10 s limit
        link = FakeLink(at([(9.0 * (i + 1), b".") for i in range(10)] + [(91.0, b"OK\r\n")]))
        steps, line = rescue_upload.read_erase_reply(link, 10.0, now=link.now)
        self.assertEqual((steps, line), (10, "OK"))

    def test_a_silence_longer_than_the_limit_is_a_timeout(self):
        link = FakeLink(at([(1.0, b"."), (2.0, b"."), (13.0, b".")]))  # an 11 s gap
        with self.assertRaisesRegex(TimeoutError, "2 erase steps"):
            rescue_upload.read_erase_reply(link, 10.0, now=link.now)

    def test_nothing_at_all_times_out(self):
        link = FakeLink([])
        with self.assertRaisesRegex(TimeoutError, "0 erase steps"):
            rescue_upload.read_erase_reply(link, 3.0, now=link.now)

    def test_blank_lines_before_the_reply_are_skipped(self):
        link = FakeLink(at([(0.1, b"\r\n"), (0.2, b"."), (0.3, b"\r\n"), (0.4, b"OK\r\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (1, "OK"))


if __name__ == "__main__":
    unittest.main()
