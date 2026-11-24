#!/usr/bin/env python3
"""Upload a signed FerroBoot image to a flash slot via the bootloader UART rescue mode.

Rescue protocol, version 2 (see crates/boot-rescue and bootloader-h750/src/main.rs,
enter_rescue_mode). Version 1 had no way to ask the bootloader anything; this tool refuses it.
  p            -> "PONG"
  i            -> one line of key=value pairs, all numbers decimal:
                  INFO proto=2 board=h750-netlite bl=1 flash=4194304 align=1 chunk=512
                       slot0=655360 slot1=655360 window=524288
                  align = the flash's program word (every 'w' must be a multiple of it),
                  chunk = the largest 'w' payload to send, window = the largest image the board
                  can run (0 if it has no such limit)
  e <slot>     -> erases the WHOLE slot, one erase operation at a time; one '.' per finished
                  step, then "OK" or "ERR"
  w <slot> <off_hex> <len_hex>\n<bytes>   (1 <= len <= chunk, off and len multiples of align,
               inside the slot) -> "OK", or ERR_SLOT / ERR_LEN / ERR_RANGE / ERR_ALIGN / ERR_WR
  m <slot>     -> verify the image in the slot, then mark it Pending in the journal
                  with the version from its header -> "OK", or ERR_VERIFY / ERR_JRN / ERR_INIT
  j            -> dump the journal (several lines; run it by hand to see the versions)
  r            -> reboot
Slot is exactly "0" or "1"; anything else is answered with ERR_SLOT.

Usage:
  pip install pyserial
  python3 rescue_upload.py --port /dev/ttyUSB0 --image app_signed.bin --slot 0
"""
import argparse
import struct
import sys
import time

import serial

PROTO = 2
INFO_KEYS = ("proto", "board", "bl", "flash", "align", "chunk", "slot0", "slot1", "window")
STEP_TIMEOUT = 15.0    # longest silence allowed between two erase steps
MARK_TIMEOUT = 20.0    # 'm' runs the full signature verification of the image


def header_version(img):
    """Version stored in the 32-byte image header (ImageHeader.ih_ver) as 'maj.min.rev+build'.

    Layout (boot-image ImageHeader, little endian): magic u32 @0, load_addr u32 @4,
    hdr_size u16 @8, protect_tlv_size u16 @10, img_size u32 @12, flags u32 @16, then
    major u8 @20, minor u8 @21, revision u16 @22, build_num u32 @24.
    """
    major, minor, rev, build = struct.unpack_from("<BBHI", img, 20)
    return f"{major}.{minor}.{rev}+{build}"


def parse_info(line):
    """Parse the bootloader's reply to 'i' into a dict (numbers as int, 'board' as str).

    Raises ValueError, with a reason fit to show the user, if it is not a version 2 INFO line.
    Keys this tool does not know are ignored, so a newer bootloader can add some.
    """
    if not line.startswith("INFO "):
        raise ValueError(f"not an INFO line (an old bootloader?): {line!r}")
    info = {}
    for item in line[5:].split(" "):
        key, eq, value = item.partition("=")
        if not eq or not key or not value:
            raise ValueError(f"malformed field {item!r} in {line!r}")
        if key in info:
            raise ValueError(f"field {key!r} given twice in {line!r}")
        if key in INFO_KEYS and key != "board":
            if not (value.isascii() and value.isdigit()):
                raise ValueError(f"field {key!r} is not a decimal number: {value!r}")
            info[key] = int(value)
        else:
            info[key] = value  # the board name, or a field this tool does not know
    missing = [k for k in INFO_KEYS if k not in info]
    if missing:
        raise ValueError(f"missing {', '.join(missing)} in {line!r}")
    return info


def check_info(info):
    """Raise ValueError unless this tool can drive a bootloader that reported `info`."""
    if info["proto"] != PROTO:
        raise ValueError(f"the bootloader speaks rescue protocol {info['proto']}, "
                         f"this tool speaks {PROTO}")
    if info["align"] < 1 or info["chunk"] < info["align"] or info["chunk"] % info["align"]:
        raise ValueError(f"unusable chunk {info['chunk']} for program word {info['align']}")


def size_problem(n, slot_len, window, align=1):
    """None if an image of n bytes can be uploaded and run; otherwise ('error'|'warning', text).

    `slot_len` and `window` come from the bootloader's INFO line (window 0: no such limit);
    the upload is padded to a whole program word, which must still fit the slot.
    """
    padded = -(-n // align) * align
    if padded > slot_len:
        return ("error", f"a slot holds at most {slot_len} bytes")
    if window and n > window:
        return ("warning", f"larger than the {window}-byte RAM window; 'm' will probably "
                           "answer ERR_VERIFY after the upload")
    return None


def plan_chunks(img, chunk, align):
    """Yield (offset, data) for the 'w' commands that upload `img`.

    Every piece is at most `chunk` bytes and a whole number of program words; only the last may
    be shorter, and it is padded with 0xFF (what erased flash holds) up to the next word.
    """
    if chunk < 1 or align < 1 or chunk % align:
        raise ValueError(f"chunk {chunk} is not a whole number of {align}-byte words")
    off = 0
    while off < len(img):
        piece = img[off:off + chunk]
        if len(piece) % align:
            piece += b"\xff" * (align - len(piece) % align)
        yield off, piece
        off += len(piece)


def read_line(ser, timeout, now=time.time):
    """Read until CRLF or timeout; return decoded text."""
    end = now() + timeout
    buf = b""
    while now() < end:
        b = ser.read(1)
        if b:
            buf += b
            if buf.endswith(b"\n"):
                break
    return buf.decode("latin-1").strip()


def read_erase_reply(ser, step_timeout, on_step=None, now=time.time):
    """Read the reply to 'e': one '.' per finished erase step, then a line ('OK' or 'ERR').

    The time limit is per step: it starts over at every byte, so a slot with many slow steps is
    fine as long as the bootloader keeps reporting. Returns (steps, line); raises TimeoutError
    if nothing arrives for `step_timeout` seconds.
    """
    steps = 0
    text = b""
    deadline = now() + step_timeout
    while now() < deadline:
        b = ser.read(1)
        if not b:
            continue
        deadline = now() + step_timeout
        if b == b".":
            steps += 1
            if on_step:
                on_step(steps)
        elif b == b"\n":
            if text:
                return steps, text.decode("latin-1")
        elif b == b"\r":
            pass
        else:
            text += b
    raise TimeoutError(f"no reply for {step_timeout} s after {steps} erase steps")


def cmd(ser, data, expect, timeout=3.0):
    ser.write(data)
    ser.flush()
    resp = read_line(ser, timeout)
    if resp != expect:
        sys.exit(f"ERROR: sent {data[:24]!r}, expected {expect!r}, got {resp!r}")
    return resp


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", required=True)
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--image", required=True)
    ap.add_argument("--slot", choices=["0", "1"], default="0")
    ap.add_argument("--step-timeout", type=float, default=STEP_TIMEOUT,
                    help="seconds of silence allowed between two erase steps")
    ap.add_argument("--no-mark", action="store_true", help="do not mark Pending")
    ap.add_argument("--no-reboot", action="store_true")
    args = ap.parse_args()

    img = open(args.image, "rb").read()
    if len(img) < 32 or int.from_bytes(img[:4], "little") != 0x96F3B83D:
        sys.exit("Not a signed image (bad header magic). Run xtask sign first.")
    # xtask sign defaults to 1.0.0+1 unless --major/--minor/--rev/--build are given, so two
    # images signed without them look identical to the bootloader's journal.
    print(f"Image {args.image}: {len(img)} bytes, version {header_version(img)}")

    ser = serial.Serial(args.port, args.baud, timeout=0.05)
    ser.reset_input_buffer()

    print("Ping...")
    cmd(ser, b"p", "PONG")
    ser.write(b"i")
    ser.flush()
    try:
        info = parse_info(read_line(ser, 3.0))
        check_info(info)
    except ValueError as e:
        sys.exit(f"ERROR: cannot talk to this bootloader: {e}")
    print(f"Bootloader: board {info['board']}, bl {info['bl']}, program word {info['align']}, "
          f"chunk {info['chunk']}, flash {info['flash']} bytes")

    slot_len = info[f"slot{args.slot}"]
    problem = size_problem(len(img), slot_len, info["window"], info["align"])
    if problem and problem[0] == "error":
        sys.exit(f"Image is {len(img)} bytes; {problem[1]}.")
    if problem:
        print(f"WARNING: image is {len(img)} bytes, {problem[1]}.")

    print(f"Erasing slot {args.slot} (whole slot; '.' = one erase step)...")
    ser.write(f"e {args.slot}\n".encode())
    ser.flush()
    try:
        steps, reply = read_erase_reply(
            ser, args.step_timeout, on_step=lambda n: print(".", end="", flush=True))
    except TimeoutError as e:
        sys.exit(f"\nERROR: {e}")
    print()
    if reply != "OK":
        sys.exit(f"ERROR: erase failed after {steps} steps: {reply!r}")

    sent = 0
    for off, piece in plan_chunks(img, info["chunk"], info["align"]):
        hdr = f"w {args.slot} {off:x} {len(piece):x}\n".encode()
        ser.write(hdr + piece)  # stop-and-wait: the bootloader polls UART, no FIFO
        ser.flush()
        resp = read_line(ser, 3.0)
        if resp != "OK":
            sys.exit(f"\nERROR at offset 0x{off:x}: {resp!r}")
        sent = off + len(piece)
        print(f"\r  {min(sent, len(img))}/{len(img)} bytes", end="", flush=True)
    print()

    if not args.no_mark:
        print("Verifying and marking slot Pending...")
        cmd(ser, f"m {args.slot}\n".encode(), "OK", timeout=MARK_TIMEOUT)
    if not args.no_reboot:
        print("Rebooting...")
        ser.write(b"r\n")
    print("Done.")


if __name__ == "__main__":
    main()
