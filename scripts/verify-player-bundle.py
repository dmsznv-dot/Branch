#!/usr/bin/env python3
"""Statically verify a hashed bundle by parsing Mach-O/PE imports; no runtime QA.

Errors contain stable codes, never user/build paths. No bundled code is run.
"""
import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct
import sys


class Rejected(Exception):
    pass


def require(condition, code):
    if not condition:
        raise Rejected(code)


def unpack(data, offset, form):
    require(offset >= 0 and offset + struct.calcsize(form) <= len(data), "native_format")
    return struct.unpack_from(form, data, offset)


def cstring(data, offset, limit):
    require(0 <= offset < limit <= len(data), "native_format")
    end = data.find(b"\0", offset, limit)
    require(end >= offset and end - offset <= 4096, "native_format")
    try:
        value = data[offset:end].decode("utf-8")
    except UnicodeDecodeError as error:
        raise Rejected("native_format") from error
    require(bool(value), "native_format")
    return value


@dataclass
class Native:
    architecture: str
    imports: list
    rpaths: list
    install_name: str | None = None


def parse_macho(data):
    magic, cpu, _, _, count, size, _, _ = unpack(data, 0, "<8I")
    require(magic == 0xFEEDFACF and size <= len(data) - 32 and count <= size // 8, "native_format")
    offset, end = 32, 32 + size
    imports, rpaths, install_name = [], [], None
    loads = {0xC, 0x80000018, 0x8000001F, 0x80000023, 0x20}
    for _ in range(count):
        command, length = unpack(data, offset, "<2I")
        require(length >= 8 and length % 4 == 0 and offset + length <= end, "native_format")
        if command in loads or command == 0xD:
            require(length >= 24, "native_format")
            name_offset, = unpack(data, offset + 8, "<I")
            require(24 <= name_offset < length, "native_format")
            name = cstring(data, offset + name_offset, offset + length)
            if command == 0xD:
                install_name = name
            else:
                imports.append(name)
        elif command == 0x8000001C:
            require(length >= 12, "native_format")
            name_offset, = unpack(data, offset + 8, "<I")
            require(12 <= name_offset < length, "native_format")
            rpaths.append(cstring(data, offset + name_offset, offset + length))
        offset += length
    require(offset == end, "native_format")
    return Native({0x0100000C: "arm64", 0x01000007: "x86_64"}.get(cpu, "unknown"), imports, rpaths, install_name)


def parse_pe(data):
    require(data[:2] == b"MZ", "native_format")
    pe_offset, = unpack(data, 0x3C, "<I")
    require(data[pe_offset:pe_offset + 4] == b"PE\0\0", "native_format")
    machine, sections, _, _, _, optional_size, _ = unpack(data, pe_offset + 4, "<HHIIIHH")
    optional = pe_offset + 24
    magic, = unpack(data, optional, "<H")
    require(magic == 0x20B and optional_size >= 112 and sections <= 96, "native_format")
    image_base, = unpack(data, optional + 24, "<Q")
    directory_count, = unpack(data, optional + 108, "<I")
    require(directory_count <= 16 and optional_size >= 112 + directory_count * 8, "native_format")
    section_rows = []
    for index in range(sections):
        row = optional + optional_size + index * 40
        _, _, virtual, raw_size, raw, _, _, _, _, _ = unpack(data, row, "<8sIIIIIIHHI")
        require(raw <= len(data) and raw_size <= len(data) - raw, "native_format")
        section_rows.append((virtual, raw, raw_size))

    def file_offset(rva, amount=1):
        for virtual, raw, raw_size in section_rows:
            delta = rva - virtual
            if 0 <= delta and amount <= raw_size - delta:
                return raw + delta
        raise Rejected("native_format")

    imports = []
    for index, stride in [(1, 20), (13, 32)]:
        if directory_count <= index:
            continue
        rva, length = unpack(data, optional + 112 + index * 8, "<II")
        if rva == 0 and length == 0:
            continue
        require(rva != 0 and stride <= length <= 1024 * 1024, "native_format")
        terminated = False
        for position in range(length // stride):
            offset = file_offset(rva + position * stride, stride)
            values = unpack(data, offset, "<" + "I" * (stride // 4))
            if not any(values):
                terminated = True
                break
            name_rva = values[3] if index == 1 else values[1]
            if index == 13:
                require(values[0] in (0, 1), "native_format")
                if values[0] == 0:
                    require(name_rva >= image_base, "native_format")
                    name_rva -= image_base
            name_offset = file_offset(name_rva)
            bound = next(raw + raw_size for _, raw, raw_size in section_rows if raw <= name_offset < raw + raw_size)
            imports.append(cstring(data, name_offset, bound))
        require(terminated, "native_format")
    return Native({0x8664: "x86_64", 0xAA64: "arm64"}.get(machine, "unknown"), imports, [])


def native_file(data, target):
    return parse_macho(data) if target == "aarch64-apple-darwin" else parse_pe(data)


def relative_path(value):
    require(isinstance(value, str) and bool(value) and "\\" not in value and ":" not in value, "inventory_path")
    path = PurePosixPath(value)
    require(not path.is_absolute() and all(part not in ("..", ".", "") for part in value.split("/")), "inventory_path")
    return value


def inside(root, name):
    path = root / relative_path(name)
    require(path.resolve().is_relative_to(root), "bundle_escape")
    return path


SYSTEM_DLLS = set("advapi32 avrt bcrypt bcryptprimitives cabinet cfgmgr32 comctl32 comdlg32 crypt32 cryptbase cryptnet cryptui d2d1 d3d11 d3d12 d3d9 dcomp dbghelp dnsapi dwmapi dxgi dxva2 gdi32 hid imm32 iphlpapi kernel32 kernelbase mf mfplat mfreadwrite mfuuid msimg32 msvcrt ncrypt netapi32 normaliz ntdll ole32 oleacc oleaut32 opengl32 powrprof propsys psapi rpcrt4 secur32 setupapi shcore shell32 shlwapi strmiids synchronization ucrtbase user32 userenv usp10 uxtheme version virtdisk winhttp wininet winmm winspool wintrust wldap32 ws2_32 wtsapi32".split())
MACHO_MAGICS = {
    bytes.fromhex(value) for value in (
        "feedface", "cefaedfe", "feedfacf", "cffaedfe",
        "cafebabe", "bebafeca", "cafebabf", "bfbafeca",
    )
}


def is_system(name, windows):
    if windows:
        lower = name.lower()
        return lower.endswith(".dll") and (lower[:-4] in SYSTEM_DLLS or lower.startswith(("api-ms-win-", "ext-ms-win-")))
    return name.startswith(("/usr/lib/", "/System/Library/")) and "/../" not in name


def inspect_bundle(target, bundle, manifest):
    require(target in ("aarch64-apple-darwin", "x86_64-pc-windows-msvc"), "unsupported_target")
    require(bundle.is_dir(), "bundle_missing")
    root = bundle.resolve()
    pin = manifest.get("targets", {}).get(target, {})
    require(pin.get("status") == "inventory_complete" and pin.get("license_inventory_complete") is True, "inventory_incomplete")
    require(pin.get("os_floor") and all(pin.get("build_evidence", {}).get(key) for key in ("os", "sdk", "compiler")), "inventory_incomplete")
    artifacts = pin.get("artifacts", [])
    require(isinstance(artifacts, list) and bool(artifacts), "inventory_incomplete")
    files, binaries = {}, {}
    windows = target == "x86_64-pc-windows-msvc"
    for artifact in artifacts:
        name = relative_path(artifact.get("file"))
        key = name.lower() if windows else name
        require(key not in files, "inventory_duplicate")
        files[key] = artifact
        path = inside(root, name)
        require(path.is_file(), "artifact_missing")
        data = path.read_bytes()
        require(re.fullmatch(r"[0-9a-f]{64}", artifact.get("sha256", "")) and hashlib.sha256(data).hexdigest() == artifact["sha256"], "artifact_hash")
        if artifact.get("kind") == "binary":
            binary = native_file(data, target)
            require(binary.architecture == ("x86_64" if windows else "arm64"), "architecture_mismatch")
            require(not re.search(rb"/(?:Users|home|workspace|private/tmp|tmp)/|[A-Za-z]:[\\/](?:Users|workspace|build|tmp)[\\/]", data), "absolute_native_path")
            binaries[name] = binary
        else:
            require(data[:2] != b"MZ" and data[:4] not in MACHO_MAGICS, "unlisted_native_binary")
    actual_files = set()
    for path in root.rglob("*"):
        require(path.resolve().is_relative_to(root), "bundle_escape")
        if path.is_file():
            name = path.relative_to(root).as_posix()
            actual_files.add(name.lower() if windows else name)
    require(actual_files == set(files), "unlisted_bundle_file")
    entry = relative_path(pin.get("entrypoint"))
    require(entry in binaries, "entrypoint_missing")
    require({"mpv", "ffmpeg"}.issubset({a.get("component") for a in artifacts if a.get("kind") == "binary"}), "required_library_missing")
    for name in pin.get("required_files", []):
        require((name.lower() if windows else name) in files, "required_library_missing")
    sources = pin.get("sources", {})
    components = {a.get("component") for a in artifacts if a.get("kind") == "binary"} - {"application"}
    require(components and components.issubset(sources), "license_inventory")
    for component in components:
        source = sources[component]
        require(isinstance(source, dict) and source.get("url", "").startswith("https://") and re.fullmatch(r"[0-9a-f]{40}", source.get("revision", "")), "license_inventory")
        require(source.get("license") not in (None, "", "unknown", "NOASSERTION") and source.get("recipe") and source.get("configure_flags"), "license_inventory")
        required_notices = source.get("notices", [])
        require(required_notices and source.get("source_offer"), "license_inventory")
        for notice in [*required_notices, source["source_offer"]]:
            key = relative_path(notice)
            require((key.lower() if windows else key) in files and inside(root, key).is_file(), "license_inventory")
    if windows:
        runtime = pin.get("webview2", {})
        require(runtime.get("mode") in ("system_verified", "offline_installer", "fixed_runtime"), "webview2_unverified")
        if runtime["mode"] == "system_verified":
            require(runtime.get("version") and runtime.get("verification"), "webview2_unverified")
        else:
            require(runtime.get("files") and all(path.lower() in files for path in runtime["files"]), "webview2_unverified")
    graph = pin.get("dependency_graph", {})
    require(isinstance(graph, dict) and set(graph) == set(binaries), "import_graph_mismatch")
    dlls = {}
    if windows:
        for name in binaries:
            base = PurePosixPath(name).name.lower()
            require(base not in dlls, "ambiguous_dependency")
            dlls[base] = name

    def expand(value, loader):
        if value.startswith("@loader_path/"):
            return root / PurePosixPath(loader).parent / value[len("@loader_path/"):]
        if value.startswith("@executable_path/"):
            return root / PurePosixPath(entry).parent / value[len("@executable_path/"):]
        raise Rejected("absolute_native_path")

    for name, binary in binaries.items():
        for value in [*binary.rpaths, *([binary.install_name] if binary.install_name else []), *binary.imports]:
            require(not value.startswith("/") or is_system(value, False), "absolute_native_path")
            require(not re.match(r"^[A-Za-z]:", value) and "\\" not in value, "absolute_native_path")
        observed = sorted(value.lower() if windows else value for value in binary.imports)
        expected = graph[name]
        require(isinstance(expected, list) and observed == sorted(value.lower() if windows else value for value in expected), "import_graph_mismatch")
        for imported in binary.imports:
            if is_system(imported, windows):
                continue
            if windows:
                require("/" not in imported and imported.lower() in dlls, "dependency_missing")
                require(PurePosixPath(dlls[imported.lower()]).parent == PurePosixPath(entry).parent, "dependency_location")
            else:
                if imported.startswith("@rpath/"):
                    rpaths = [(value, name) for value in binary.rpaths] + [(value, entry) for value in binaries[entry].rpaths]
                    candidates = [expand(value, loader) / imported[7:] for value, loader in rpaths]
                else:
                    candidates = [expand(imported, name)]
                require(all(path.resolve().is_relative_to(root) for path in candidates), "bundle_escape")
                require(any(path.is_file() and path.resolve().relative_to(root).as_posix() in binaries for path in candidates), "dependency_missing")
    return {"target": target, "binaries": len(binaries), "artifacts": len(artifacts), "status": "bundle_static_verified", "runtime_tested": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).resolve().parents[1] / "docs/development/native-dependencies.json")
    args = parser.parse_args()
    try:
        result = inspect_bundle(args.target, args.bundle, json.loads(args.manifest.read_text()))
    except Rejected as error:
        print("bundle_rejected:" + str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, TypeError, KeyError, struct.error):
        print("bundle_rejected:invalid_inventory_or_native_file", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
