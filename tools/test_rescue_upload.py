"""Host-side checks for rescue_upload.py (no serial port needed): python3 -m unittest tools/test_rescue_upload.py"""
import os
import random
import sys
import time
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

    def test_the_whole_line_is_consumed_so_the_next_reply_is_read_cleanly(self):
        # What the board sends for 'e' and then for the first 'w'. Stopping the erase reply at the
        # '\r' leaves the '\n' behind, and the next read_line returns '' at once ("ERROR at
        # offset 0x0: ''" on the first board run).
        link = FakeLink(at([(0.1, b".."), (0.2, b"OK\r\n"), (0.5, b"OK\r\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (2, "OK"))
        self.assertEqual(rescue_upload.read_line(link, 3.0, now=link.now), "OK")
        self.assertEqual(link.arrivals, [], "everything the board sent has been read")

    def test_a_reply_that_is_only_a_newline_ends_at_the_newline(self):
        link = FakeLink(at([(0.1, b"."), (0.2, b"OK\n"), (0.5, b"OK\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (1, "OK"))
        self.assertEqual(rescue_upload.read_line(link, 3.0, now=link.now), "OK")

    def test_blank_lines_before_the_reply_are_skipped(self):
        link = FakeLink(at([(0.1, b"\r\n"), (0.2, b"."), (0.3, b"\r\n"), (0.4, b"OK\r\n")]))
        self.assertEqual(rescue_upload.read_erase_reply(link, 5.0, now=link.now), (1, "OK"))


class FakeBoard:
    """A bootloader's rescue mode as seen over the wire, byte for byte as the firmware answers.

    Replies end in CRLF, `e` prints one '.' per erase step then OK, `w` takes its payload after the
    header line and checks range and alignment. The flash starts erased (0xFF).
    """

    def __init__(self, info, slot_len=655360, steps=10, drop_erase_reply_after=None,
                 erase_error_after=None):
        self.info = info
        self.slot_len = slot_len
        self.steps = steps
        self.drop_after = drop_erase_reply_after  # stall after this many dots
        self.error_after = erase_error_after      # answer ERR after this many dots
        self.flash = {0: bytearray(b"\xff" * slot_len), 1: bytearray(b"\xff" * slot_len)}
        self.rx = bytearray()   # what the host has sent and the board has not parsed yet
        self.tx = bytearray()   # what the board has sent and the host has not read yet
        self.log = []
        self.marked = []
        self.unread_at_reboot = None

    # --- the serial port the tool sees ---
    def write(self, data):
        self.rx += data
        self._run()

    def flush(self):
        pass

    def reset_input_buffer(self):
        self.tx.clear()

    def read(self, n):
        if self.tx:
            return bytes([self.tx.pop(0)])
        time.sleep(0.001)
        return b""

    # --- the board ---
    def _line(self):
        i = self.rx.find(b"\n")
        if i < 0:
            return None
        line = bytes(self.rx[:i]).decode()
        del self.rx[:i + 1]
        return line

    def _run(self):
        while self.rx:
            c = chr(self.rx[0])
            if c in "pir":
                del self.rx[0]
                self.log.append(c)
                if c == "r":
                    self.unread_at_reboot = bytes(self.tx)
                self.tx += {"p": b"PONG\r\n", "i": self.info.encode() + b"\r\n", "r": b"REBOOT\r\n"}[c]
            elif c in "em":
                line = self._line()
                if line is None:
                    return
                self.log.append(line)
                slot = int(line.split()[1])
                if slot not in (0, 1):
                    self.tx += b"ERR_SLOT\r\n"
                    continue
                if c == "m":
                    self.marked.append(slot)
                    self.tx += b"OK\r\n"
                    continue
                self.flash[slot][:] = b"\xff" * self.slot_len
                n = self.steps if self.drop_after is None else self.drop_after
                if self.error_after is not None:
                    self.tx += b"." * self.error_after + b"ERR\r\n"
                    continue
                self.tx += b"." * n
                if self.drop_after is None:
                    self.tx += b"OK\r\n"
            elif c == "w":
                head = bytes(self.rx)
                i = head.find(b"\n")
                if i < 0:
                    return
                _, slot, off, ln = head[:i].decode().split()
                slot, off, ln = int(slot), int(off, 16), int(ln, 16)
                if len(self.rx) < i + 1 + ln:
                    return
                payload = bytes(self.rx[i + 1:i + 1 + ln])
                del self.rx[:i + 1 + ln]
                self.log.append(f"w {slot} {off:x} {ln:x}")
                align = int(self.info.split(" align=")[1].split()[0])
                chunk = int(self.info.split(" chunk=")[1].split()[0])
                if slot not in (0, 1):
                    self.tx += b"ERR_SLOT\r\n"
                elif ln == 0 or ln > chunk:
                    self.tx += b"ERR_LEN\r\n"
                elif off + ln > self.slot_len:
                    self.tx += b"ERR_RANGE\r\n"
                elif off % align or ln % align:
                    self.tx += b"ERR_ALIGN\r\n"
                else:
                    self.flash[slot][off:off + ln] = payload
                    self.tx += b"OK\r\n"
            else:
                del self.rx[0]  # like the firmware: ignore stray bytes


class Probe(unittest.TestCase):
    def run_probe(self, board):
        import contextlib
        import io
        with contextlib.redirect_stdout(io.StringIO()):
            return rescue_upload.run_probe(board, rescue_upload.parse_info(board.info))

    def test_a_correct_bootloader_refuses_every_case_and_nothing_is_written(self):
        board = FakeBoard(H750_INFO)
        self.assertEqual(self.run_probe(board), [])
        self.assertEqual(board.marked, [])
        for slot in (0, 1):
            self.assertTrue(all(b == 0xFF for b in board.flash[slot]))
        self.assertFalse(any(x.startswith("e ") and x != "e 2" for x in board.log))

    def test_every_reply_the_probe_expects_is_distinct_per_cause(self):
        cases = rescue_upload.probe_cases(rescue_upload.parse_info(H750_INFO))
        self.assertEqual({e for _, _, e in cases}, {"ERR_SLOT", "ERR_RANGE", "ERR_LEN"})

    def test_a_word_programmed_board_also_gets_an_alignment_case(self):
        info = H750_INFO.replace("align=1", "align=4")
        names = [n for n, _, _ in rescue_upload.probe_cases(rescue_upload.parse_info(info))]
        self.assertIn("w: offset not a whole word", names)

    def test_a_wrong_reply_is_reported(self):
        board = FakeBoard(H750_INFO)
        board.slot_len = 10 ** 9   # this "bootloader" does not check the range
        failed = self.run_probe(board)
        self.assertEqual([f[0] for f in failed][:1], ["w: offset at the end of the slot"])


class MainHarness(unittest.TestCase):
    """Runs rescue_upload.main() against a FakeBoard. No tests of its own."""

    def setUp(self):
        import tempfile
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def image(self, size):
        body = bytes((i * 7 + 3) & 0xFF for i in range(size - 32))
        path = os.path.join(self.tmp.name, f"img{size}.bin")
        with open(path, "wb") as f:
            f.write(XTASK_HEADER_4_2_3_B7 + body)
        return path, XTASK_HEADER_4_2_3_B7 + body

    def upload(self, board, path, *extra, slot="0"):
        argv = ["rescue_upload.py", "--port", "fake", "--image", path, "--slot", slot, *extra]
        fake_serial = types.SimpleNamespace(Serial=lambda *a, **k: board)
        old = (sys.argv, rescue_upload.serial)
        sys.argv, rescue_upload.serial = argv, fake_serial
        try:
            rescue_upload.main()
        finally:
            sys.argv, rescue_upload.serial = old

    def quiet(self):
        import contextlib
        import io
        return contextlib.redirect_stdout(io.StringIO())


class EndToEnd(MainHarness):
    """rescue_upload.main() against FakeBoard: the exchange as a whole, not just its helpers."""

    def test_a_whole_upload_to_the_h750_ends_with_the_image_in_the_slot_and_marked(self):
        path, img = self.image(4584)  # the size from the board run that found the CRLF bug
        board = FakeBoard(H750_INFO)
        with self.quiet():
            self.upload(board, path)
        self.assertEqual(bytes(board.flash[0][:len(img)]), img)
        self.assertTrue(all(b == 0xFF for b in board.flash[0][len(img):]))
        self.assertEqual(board.marked, [0])
        self.assertEqual(board.log[:3], ["p", "i", "e 0"])
        self.assertEqual(board.log[-1], "r")
        # Every reply the board had sent was read before the tool asked for the reboot. A byte
        # left behind (the '\n' of an 'OK\r\n') would make the next read_line return '' at once.
        self.assertEqual(board.unread_at_reboot, b"")
        writes = [x for x in board.log if x.startswith("w ")]
        self.assertEqual(writes[0], "w 0 0 200")
        self.assertEqual(len(writes), 9)  # 4584 = 8 x 512 + 488

    def test_slot_1_is_erased_written_and_marked_and_slot_0_is_left_alone(self):
        path, img = self.image(4584)
        board = FakeBoard(H750_INFO)
        board.flash[0][:4] = b"keep"
        with self.quiet():
            self.upload(board, path, slot="1")
        self.assertEqual(bytes(board.flash[1][:len(img)]), img)
        self.assertEqual(bytes(board.flash[0][:4]), b"keep")
        self.assertEqual(board.marked, [1])
        self.assertIn("e 1", board.log)
        self.assertTrue(all(x.startswith("w 1 ") for x in board.log if x.startswith("w ")))

    def test_no_mark_and_no_reboot_are_honoured(self):
        path, img = self.image(4584)
        board = FakeBoard(H750_INFO)
        with self.quiet():
            self.upload(board, path, "--no-mark", "--no-reboot")
        self.assertEqual(bytes(board.flash[0][:len(img)]), img)
        self.assertEqual(board.marked, [])
        self.assertNotIn("r", board.log)

    def test_a_word_programmed_board_gets_a_padded_last_chunk(self):
        info = H750_INFO.replace("align=1", "align=4")
        path, img = self.image(4585)  # not a multiple of 4
        board = FakeBoard(info)
        with self.quiet():
            self.upload(board, path)
        self.assertEqual(bytes(board.flash[0][:len(img)]), img)
        self.assertEqual(bytes(board.flash[0][len(img):len(img) + 3]), b"\xff" * 3)
        self.assertEqual(board.marked, [0])
        # 4585 = 8 x 512 + 489; the last piece is rounded up to a whole 4-byte word (492 = 0x1ec)
        last_off = 4585 // 512 * 512
        last_len = -(-(4585 - last_off) // 4) * 4
        self.assertEqual((last_off, last_len), (0x1000, 0x1EC))
        self.assertEqual([x for x in board.log if x.startswith("w ")][-1], f"w 0 {last_off:x} {last_len:x}")

    def test_an_old_bootloader_is_refused_before_anything_is_erased(self):
        path, _ = self.image(4584)
        board = FakeBoard("[RESCUE] BL v1.0, STM32H750, NOR BY25Q32ES")
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path)
        self.assertIn("cannot talk to this bootloader", str(cm.exception))
        self.assertEqual(board.log, ["p", "i"])

    def test_another_protocol_version_is_refused_before_anything_is_erased(self):
        path, _ = self.image(4584)
        board = FakeBoard(H750_INFO.replace("proto=2", "proto=3"))
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path)
        self.assertIn("protocol 3", str(cm.exception))
        self.assertEqual(board.log, ["p", "i"])

    def test_an_image_bigger_than_the_slot_is_refused_before_the_erase(self):
        path, _ = self.image(655360 + 1)
        board = FakeBoard(H750_INFO)
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path)
        self.assertIn("a slot holds at most 655360", str(cm.exception))
        self.assertEqual(board.log, ["p", "i"])

    def test_an_erase_that_fails_stops_the_upload_before_any_write(self):
        path, _ = self.image(4584)
        board = FakeBoard(H750_INFO, erase_error_after=2)
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path)
        self.assertIn("erase failed after 2 steps", str(cm.exception))
        self.assertIn("'ERR'", str(cm.exception))
        self.assertFalse([x for x in board.log if x.startswith("w ")])
        self.assertEqual(board.marked, [])

    def test_the_chunk_size_comes_from_the_info_line(self):
        path, img = self.image(1000)
        board = FakeBoard(H750_INFO.replace("chunk=512", "chunk=128"))  # refuses anything bigger
        with self.quiet():
            self.upload(board, path)
        self.assertEqual(bytes(board.flash[0][:len(img)]), img)
        lens = [int(x.split()[3], 16) for x in board.log if x.startswith("w ")]
        self.assertEqual(lens, [128] * 7 + [104])  # 1000 = 7 x 128 + 104

    def test_the_slot_size_comes_from_the_slot_asked_for(self):
        path, _ = self.image(4584)
        info = H750_INFO.replace("slot1=655360", "slot1=4096")
        board = FakeBoard(info)
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path, slot="1")
        self.assertIn("a slot holds at most 4096", str(cm.exception))
        self.assertEqual(board.log, ["p", "i"])
        # the same image is fine for slot 0
        board = FakeBoard(info)
        with self.quiet():
            self.upload(board, path, slot="0")
        self.assertEqual(board.marked, [0])

    def test_an_erase_that_goes_silent_is_reported_by_the_step_timeout(self):
        path, _ = self.image(4584)
        board = FakeBoard(H750_INFO, drop_erase_reply_after=3)
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path, "--step-timeout", "0.3")
        self.assertIn("3 erase steps", str(cm.exception))
        self.assertNotIn("m 0", board.log)

    def test_a_refused_write_stops_the_upload_with_the_boards_reply(self):
        # a board whose slot is smaller than it said: the first write inside it works, a later one
        # is refused with ERR_RANGE
        path, _ = self.image(4584)
        board = FakeBoard(H750_INFO, slot_len=2048)
        with self.quiet(), self.assertRaises(SystemExit) as cm:
            self.upload(board, path)
        self.assertIn("'ERR_RANGE'", str(cm.exception))
        self.assertIn("0x800", str(cm.exception))
        self.assertEqual(board.marked, [])


class ProbeEndToEnd(MainHarness):
    """`--probe` through main(): the arguments, the preamble, and the exit status."""

    def run_main(self, board, *extra):
        argv = ["rescue_upload.py", "--port", "fake", *extra]
        old = (sys.argv, rescue_upload.serial)
        sys.argv, rescue_upload.serial = argv, types.SimpleNamespace(Serial=lambda *a, **k: board)
        try:
            with self.quiet():
                rescue_upload.main()
        finally:
            sys.argv, rescue_upload.serial = old

    def test_probe_needs_no_image_and_reaches_the_board(self):
        # The bug this guards: main() checked the image header even with --probe (no image), and
        # exited with "Not a signed image" before sending anything.
        board = FakeBoard(H750_INFO)
        with self.assertRaises(SystemExit) as cm:
            self.run_main(board, "--probe")
        self.assertEqual(cm.exception.code, 0, "all probes refused as expected: exit status 0")
        self.assertEqual(board.log[:2], ["p", "i"])
        self.assertGreater(len(board.log), 2, "the probe cases were sent")

    def test_probe_changes_nothing_on_the_board(self):
        board = FakeBoard(H750_INFO)
        with self.assertRaises(SystemExit):
            self.run_main(board, "--probe")
        self.assertEqual(board.marked, [])
        self.assertNotIn("r", board.log)
        for slot in (0, 1):
            self.assertTrue(all(b == 0xFF for b in board.flash[slot]))
        # the only erase or mark it may send is the refused one, for slot 2
        self.assertEqual([x for x in board.log if x[:1] in "em"], ["e 2", "m 2"])

    def test_a_probe_that_fails_ends_with_a_message_and_a_failing_status(self):
        board = FakeBoard(H750_INFO)
        board.slot_len = 10 ** 9  # a board that does not check the range
        with self.assertRaises(SystemExit) as cm:
            self.run_main(board, "--probe")
        self.assertIn("probe(s) failed", str(cm.exception.code))

    def test_probe_still_refuses_an_old_bootloader(self):
        board = FakeBoard("[RESCUE] BL v1.0, STM32H750, NOR BY25Q32ES")
        with self.assertRaises(SystemExit) as cm:
            self.run_main(board, "--probe")
        self.assertIn("cannot talk to this bootloader", str(cm.exception.code))
        self.assertEqual(board.log, ["p", "i"])

    def test_without_probe_an_image_is_still_required_and_checked(self):
        with self.assertRaises(SystemExit) as cm:
            self.run_main(FakeBoard(H750_INFO))
        self.assertEqual(cm.exception.code, 2, "argparse: --image is required unless --probe")
        path = os.path.join(self.tmp.name, "junk.bin")
        open(path, "wb").write(b"\x00" * 64)
        with self.assertRaises(SystemExit) as cm:
            self.run_main(FakeBoard(H750_INFO), "--image", path)
        self.assertIn("Not a signed image", str(cm.exception.code))


if __name__ == "__main__":
    unittest.main()
