#!/usr/bin/env python3
"""Upload a signed FerroBoot image to a NOR slot via the bootloader UART rescue mode.

Rescue protocol (see bootloader-h750/src/main.rs, enter_rescue_mode):
  p            -> "PONG"
  e <slot>     -> erases the WHOLE slot (every 64KB block, takes seconds) -> "OK"
  w <slot> <off_hex> <len_hex>\n<bytes>   (1 <= len <= 512, must stay inside the slot)
               -> "OK", or ERR_SLOT / ERR_LEN / ERR_RANGE / ERR_WR
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
import sys
import time

import serial

CHUNK = 512
# A slot is 640 KiB on the H750 NetLite (boot-layout H750_NETLITE); the bootloader refuses
# writes outside the slot, and 'm' refuses an image that does not verify.
MAX_IMAGE = 0xA0000
ERASE_TIMEOUT = 40.0   # 10 blocks x up to ~2 s (NOR block erase), plus margin
MARK_TIMEOUT = 20.0    # 'm' runs the full signature verification of the image


def read_line(ser, timeout):
    """Read until CRLF or timeout; return decoded text."""
    end = time.time() + timeout
    buf = b""
    while time.time() < end:
        b = ser.read(1)
        if b:
            buf += b
            if buf.endswith(b"\n"):
                break
    return buf.decode("latin-1").strip()


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
    ap.add_argument("--no-mark", action="store_true", help="do not mark Pending")
    ap.add_argument("--no-reboot", action="store_true")
    args = ap.parse_args()

    img = open(args.image, "rb").read()
    if len(img) > MAX_IMAGE:
        sys.exit(f"Image is {len(img)} bytes; a slot holds at most {MAX_IMAGE}.")
    if int.from_bytes(img[:4], "little") != 0x96F3B83D:
        sys.exit("Not a signed image (bad header magic). Run xtask sign first.")

    ser = serial.Serial(args.port, args.baud, timeout=0.05)
    ser.reset_input_buffer()

    print("Ping...")
    cmd(ser, b"p", "PONG")
    print(f"Erasing slot {args.slot} (whole slot, up to ~{int(ERASE_TIMEOUT)} s)...")
    cmd(ser, f"e {args.slot}\n".encode(), "OK", timeout=ERASE_TIMEOUT)

    off = 0
    while off < len(img):
        chunk = img[off:off + CHUNK]
        hdr = f"w {args.slot} {off:x} {len(chunk):x}\n".encode()
        ser.write(hdr + chunk)  # stop-and-wait: the bootloader polls UART, no FIFO
        ser.flush()
        resp = read_line(ser, 3.0)
        if resp != "OK":
            sys.exit(f"\nERROR at offset 0x{off:x}: {resp!r}")
        off += len(chunk)
        print(f"\r  {off}/{len(img)} bytes", end="", flush=True)
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
