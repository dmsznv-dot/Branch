"""Test-only crafted references. Never a decoder implementation.

Synthesized source material is CC0. Native unlocked positive controls prove
MOV alis drefs and Matroska ordered chapters can reach the watched spy files.
"""
import ctypes
import os
import pathlib
import struct
import subprocess
import sys
import time

fixtures, out = map(pathlib.Path, sys.argv[1:])
original = (fixtures / "h264.mp4").read_bytes()
spy = out / "spy.mp4"
spy.write_bytes(original)


def rewrite_atoms(data):
    result, pos = b"", 0
    while pos < len(data):
        size, kind = struct.unpack(">I4s", data[pos:pos + 8])
        assert size >= 8 and pos + size <= len(data)
        payload = data[pos + 8:pos + size]
        if kind in [b"moov", b"trak", b"mdia", b"minf", b"dinf"]:
            payload = rewrite_atoms(payload)
        if kind == b"dref":
            path = os.fsencode(spy.resolve())
            path += b"\0" * (len(path) % 2)
            alias = (b"\0" * 10 + b"\0" + b"\0" * 27 + b"\0" * 12
                     + b"\0" + b"\0" * 63 + b"\0" * 16
                     + struct.pack(">hh", -1, -1) + b"\0" * 16
                     + struct.pack(">hH", 2, len(path)) + path
                     + struct.pack(">hH", -1, 0))
            entry = b"\0" * 4 + alias
            payload = (b"\0\0\0\0\0\0\0\1"
                       + struct.pack(">I4s", len(entry) + 8, b"alis") + entry)
        result += struct.pack(">I4s", len(payload) + 8, kind) + payload
        pos += size
    return result


mov = out / "dref.mov"
mov.write_bytes(rewrite_atoms(original))


def vint(data, pos, identifier=False):
    n = 1
    while not data[pos] & (1 << (8 - n)):
        n += 1
    value = int.from_bytes(data[pos:pos + n], "big")
    return (value if identifier else value & ((1 << (7 * n)) - 1)), n


def size_bytes(size):
    for n in range(1, 9):
        if size < (1 << (7 * n)) - 1:
            return ((1 << (7 * n)) | size).to_bytes(n, "big")
    raise AssertionError("size")


def element(identifier, payload):
    return bytes.fromhex(identifier) + size_bytes(len(payload)) + payload


mkv = bytearray((fixtures / "tracks.mkv").read_bytes())
uid = bytes.fromhex("00112233445566778899aabbccddeeff")
spy_mkv = bytearray(mkv)
uid_pos = spy_mkv.index(bytes.fromhex("73a490")) + 3
spy_mkv[uid_pos:uid_pos + 16] = uid
(out / "spy.mkv").write_bytes(spy_mkv)
chapter = element("b6", element("73c4", b"\x01") + element("91", b"\0")
                  + element("92", (3_000_000_000).to_bytes(4, "big"))
                  + element("6e67", uid))
chapters = element("1043a770", element("45b9", element("45bc", b"\x01")
                   + element("45dd", b"\x01") + chapter))
pos = 0
while True:
    kind, n = vint(mkv, pos, True)
    size, m = vint(mkv, pos + n)
    if kind == 0x18538067:
        pos += n + m
        break
    pos += n + m + size
while True:
    kind, n = vint(mkv, pos, True)
    size, m = vint(mkv, pos + n)
    total = n + m + size
    if kind == 0xEC:
        remaining = total - len(chapters)
        padding = None
        for length in range(remaining):
            candidate = element("ec", b"\0" * length)
            if len(candidate) == remaining:
                padding = candidate
                break
        assert padding is not None
        mkv[pos:pos + total] = chapters + padding
        break
    pos += total
ordered = out / "ordered.mkv"
ordered.write_bytes(mkv)

# Linux IN_OPEN gives evidence of actual open, including failed subsequent decode.
libc = ctypes.CDLL(None)
libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]


def watch(path):
    fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    assert fd >= 0 and libc.inotify_add_watch(fd, os.fsencode(path), 0x20) >= 0
    return fd


def saw_open(fd):
    try:
        return len(os.read(fd, 65536)) > 0
    except BlockingIOError:
        return False
    finally:
        os.close(fd)


fd = watch(spy)
r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "quiet", "-enable_drefs", "1",
                    "-use_absolute_path", "1", "-i", str(mov), "-f", "null", "-"],
                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=5)
assert r.returncode == 0 and saw_open(fd), "mov_spy_positive_control_failed"

# libmpv's native MKV demuxer follows ordered-chapter SegmentUID siblings when
# unlocked. Production forces lavf and access-references=no instead.
fd = watch(out / "spy.mkv")
mpv = ctypes.CDLL("libmpv.so")
mpv.mpv_create.restype = ctypes.c_void_p
mpv.mpv_set_option_string.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]
mpv.mpv_initialize.argtypes = [ctypes.c_void_p]
mpv.mpv_command.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_char_p)]
mpv.mpv_wait_event.argtypes = [ctypes.c_void_p, ctypes.c_double]
mpv.mpv_terminate_destroy.argtypes = [ctypes.c_void_p]
h = mpv.mpv_create()
assert h
for key, value in [(b"config", b"no"), (b"load-scripts", b"no"), (b"terminal", b"no"),
                   (b"msg-level", b"all=no"), (b"vo", b"null"), (b"ao", b"null"),
                   (b"demuxer", b"mkv"), (b"access-references", b"yes")]:
    assert mpv.mpv_set_option_string(h, key, value) == 0
assert mpv.mpv_initialize(h) == 0
args = (ctypes.c_char_p * 3)(b"loadfile", os.fsencode(ordered), None)
assert mpv.mpv_command(h, args) >= 0
for _ in range(40):
    mpv.mpv_wait_event(h, 0.025)
mpv.mpv_terminate_destroy(h)
assert saw_open(fd), "mkv_spy_positive_control_failed"
print("resource_positive_controls_passed")
