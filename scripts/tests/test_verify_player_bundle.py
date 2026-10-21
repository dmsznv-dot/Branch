"""Exercise bundle admission against real synthetic Mach-O/PE import tables."""
import copy
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
VERIFY = ROOT / "scripts/verify-player-bundle.py"


def macho(imports=(), cpu=0x0100000C, rpaths=()):
    commands = []
    for name in imports:
        value = name.encode() + b"\0"
        size = (24 + len(value) + 7) & ~7
        commands.append(struct.pack("<6I", 0xC, size, 24, 0, 0, 0) + value + b"\0" * (size - 24 - len(value)))
    for name in rpaths:
        value = name.encode() + b"\0"
        size = (12 + len(value) + 7) & ~7
        commands.append(struct.pack("<3I", 0x8000001C, size, 12) + value + b"\0" * (size - 12 - len(value)))
    body = b"".join(commands)
    return struct.pack("<8I", 0xFEEDFACF, cpu, 0, 2, len(commands), len(body), 0, 0) + body


def pe(imports=(), machine=0x8664, delayed=()):
    # PE32+ with an .rdata section. Both import directories contain RVA-based
    # descriptors exactly as a linker emits; no test-only import-list sidecar.
    data = bytearray(0xA00)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, machine, 1, 0, 0, 0, 240, 0x2022)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<Q", data, optional + 24, 0x140000000)
    struct.pack_into("<I", data, optional + 108, 16)
    struct.pack_into("<8sIIIIIIHHI", data, optional + 240, b".rdata\0\0", 0x800, 0x1000, 0x800, 0x200, 0, 0, 0, 0, 0x40000040)
    cursor = 0x600
    for names, index, start, stride in [(imports, 1, 0x200, 20), (delayed, 13, 0x400, 32)]:
        if names:
            struct.pack_into("<II", data, optional + 112 + index * 8, 0x1000 + start - 0x200, (len(names) + 1) * stride)
        for number, name in enumerate(names):
            encoded = name.encode() + b"\0"
            rva = 0x1000 + cursor - 0x200
            if index == 1:
                struct.pack_into("<5I", data, start + number * stride, 0, 0, 0, rva, 0)
            else:
                struct.pack_into("<8I", data, start + number * stride, 1, rva, 0, 0, 0, 0, 0, 0)
            data[cursor:cursor + len(encoded)] = encoded
            cursor += len(encoded)
    return bytes(data)


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        self.manifest = self.root / "manifest.json"

    def fixture(self, target="aarch64-apple-darwin"):
        self.target = target
        windows = target.startswith("x86_64")
        self.entry = "Branch.exe" if windows else "Contents/MacOS/Branch"
        self.mpv = "libmpv-2.dll" if windows else "Contents/Frameworks/libmpv.2.dylib"
        self.ffmpeg = "avcodec-62.dll" if windows else "Contents/Frameworks/libavcodec.62.dylib"
        imports = ["libmpv-2.dll", "KERNEL32.dll"] if windows else ["@rpath/libmpv.2.dylib", "/usr/lib/libSystem.B.dylib"]
        libimports = ["avcodec-62.dll"] if windows else ["@loader_path/libavcodec.62.dylib"]
        self.files = {self.entry: pe(imports) if windows else macho(imports, rpaths=["@executable_path/../Frameworks"]), self.mpv: pe(libimports) if windows else macho(libimports), self.ffmpeg: pe() if windows else macho(), "licenses/NOTICE.txt": b"Synthetic fixture, never distributed.\n", "licenses/SOURCE-OFFER.txt": b"Corresponding source and build instructions included in inventory.\n"}
        sources = {}
        for component in ["mpv", "ffmpeg"]:
            sources[component] = {"url": "https://example.invalid/" + component + ".git", "revision": "a" * 40, "license": "GPL-2.0-or-later", "configure_flags": ["--enable-shared"], "recipe": "synthetic-test", "notices": ["licenses/NOTICE.txt"], "source_offer": "licenses/SOURCE-OFFER.txt"}
        self.pin = {"status": "inventory_complete", "license_inventory_complete": True, "os_floor": "Windows 10 1809" if windows else "macOS 11.0", "build_evidence": {"os": "synthetic", "sdk": "synthetic", "compiler": "synthetic"}, "entrypoint": self.entry, "sources": sources, "artifacts": [], "dependency_graph": {self.entry: imports, self.mpv: libimports, self.ffmpeg: []}}
        if windows:
            self.pin["webview2"] = {"mode": "system_verified", "version": "synthetic", "verification": "synthetic unit fixture"}
        for name, data in self.files.items():
            path = self.bundle / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            component = "application" if name == self.entry else "mpv" if name == self.mpv else "ffmpeg" if name == self.ffmpeg else "notices"
            self.pin["artifacts"].append({"file": name, "kind": "binary" if name in self.pin["dependency_graph"] else "notice", "component": component, "sha256": hashlib.sha256(data).hexdigest()})

    def run_verify(self):
        self.manifest.write_text(json.dumps({"schema_version": 1, "targets": {self.target: self.pin}}))
        return subprocess.run([sys.executable, str(VERIFY), "--target", self.target, "--bundle", str(self.bundle), "--manifest", str(self.manifest)], capture_output=True, text=True)

    def update_binary(self, name, data):
        (self.bundle / name).write_bytes(data)
        next(x for x in self.pin["artifacts"] if x["file"] == name)["sha256"] = hashlib.sha256(data).hexdigest()

    def assert_rejected(self, code):
        result = self.run_verify()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(code, result.stderr)
        self.assertNotIn(str(self.root), result.stderr)

    def test_complete_macho_bundle_passes(self):
        self.fixture()
        result = self.run_verify()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_complete_pe_bundle_passes(self):
        self.fixture("x86_64-pc-windows-msvc")
        result = self.run_verify()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_libmpv_is_rejected(self):
        self.fixture()
        (self.bundle / self.mpv).unlink()
        self.assert_rejected("artifact_missing")

    def test_missing_transitive_dependency_is_rejected(self):
        self.fixture()
        (self.bundle / self.ffmpeg).unlink()
        self.assert_rejected("artifact_missing")

    def test_wrong_cpu_is_rejected(self):
        self.fixture()
        self.update_binary(self.ffmpeg, macho(cpu=0x01000007))
        self.assert_rejected("architecture_mismatch")

    def test_hash_mismatch_is_rejected(self):
        self.fixture()
        (self.bundle / self.ffmpeg).write_bytes(macho() + b"changed")
        self.assert_rejected("artifact_hash")

    def test_absolute_build_path_in_macho_import_is_rejected(self):
        self.fixture()
        self.update_binary(self.mpv, macho(["/Users/builder/sdk/libavcodec.62.dylib"]))
        self.assert_rejected("absolute_native_path")

    def test_parser_detects_unlisted_macho_import(self):
        self.fixture()
        self.update_binary(self.mpv, macho(["@loader_path/libnotshipped.dylib"]))
        self.assert_rejected("import_graph_mismatch")

    def test_parser_detects_unlisted_pe_import(self):
        self.fixture("x86_64-pc-windows-msvc")
        self.update_binary(self.mpv, pe(["notshipped.dll"]))
        self.assert_rejected("import_graph_mismatch")

    def test_pe_delay_import_is_checked(self):
        self.fixture("x86_64-pc-windows-msvc")
        self.update_binary(self.mpv, pe(["avcodec-62.dll"], delayed=["notshipped.dll"]))
        self.assert_rejected("import_graph_mismatch")

    def test_inventory_cannot_waive_a_missing_import(self):
        self.fixture("x86_64-pc-windows-msvc")
        self.update_binary(self.mpv, pe(["notshipped.dll"]))
        self.pin["dependency_graph"][self.mpv] = ["notshipped.dll"]
        self.assert_rejected("dependency_missing")

    def test_incomplete_inventory_is_rejected(self):
        self.fixture()
        self.pin["status"] = "blocked_pending_native_build_and_inventory"
        self.assert_rejected("inventory_incomplete")

    def test_incomplete_license_inventory_is_rejected(self):
        self.fixture()
        self.pin["sources"]["ffmpeg"].pop("source_offer")
        self.assert_rejected("license_inventory")

    def test_outside_symlink_is_rejected(self):
        self.fixture()
        external = self.root / "external.dylib"
        external.write_bytes(macho())
        (self.bundle / self.ffmpeg).unlink()
        (self.bundle / self.ffmpeg).symlink_to(external)
        self.assert_rejected("bundle_escape")

    def test_malformed_native_file_is_rejected(self):
        self.fixture("x86_64-pc-windows-msvc")
        self.update_binary(self.mpv, b"MZbroken")
        self.assert_rejected("native_format")

    def test_native_formats_cannot_be_hidden_as_notices(self):
        self.fixture()
        notice = "licenses/NOTICE.txt"
        # Unsupported native variants must be rejected, rather than bypassing
        # architecture/import checks by changing the inventory kind to notice.
        for magic in [
            b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
            b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
            b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
            b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
        ]:
            with self.subTest(magic=magic.hex()):
                self.update_binary(notice, magic + bytes(64))
                self.assert_rejected("unlisted_native_binary")

    def test_windows_payload_rejects_actual_case_collisions(self):
        self.fixture("x86_64-pc-windows-msvc")
        alias=self.bundle/"LIBMPV-2.DLL"
        if alias.exists():
            self.skipTest("filesystem cannot represent two case-colliding files")
        alias.write_bytes(b"unreviewed alternate payload")
        self.assert_rejected("bundle_case_collision")


if __name__ == "__main__":
    unittest.main()
