"""Release safety checks against synthetic multi-platform SDK archives."""
import hashlib
import io
import json
from pathlib import Path
import plistlib
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import publish_release as release
import sdk


def binary(target):
    arm = target.startswith("aarch64")
    if "windows" in target:
        data = bytearray(128)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 64)
        data[64:68] = b"PE\0\0"
        struct.pack_into("<H", data, 68, 0xAA64 if arm else 0x8664)
    elif "apple" in target:
        data = b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x100000C if arm else 0x1000007)
    else:
        data = bytearray(64)
        data[:4] = b"\x7fELF"
        struct.pack_into("<H", data, 18, 183 if arm else 62)
    return bytes(data)


def fixtures(directory, *, bad_profile=False, wrong_arch=False):
    for name in release.expected_assets("1.2.3"):
        root = name.removesuffix(".zip").removesuffix(".tar.gz")
        files = {"LICENSE-MIT": b"MIT", "LICENSE-APACHE": b"Apache", "THIRD-PARTY.json": b"[]"}
        if "apple-xcframework" in name:
            files.update({"BUILD.txt": b"version=1.2.3\nvariant=native\nprofile=release\n", "Package.swift": b""})
            entries = []
            for identifier, platform, architectures, variant in (
                ("macos-arm64_x86_64", "macos", ["arm64", "x86_64"], ""),
                ("ios-arm64", "ios", ["arm64"], ""),
                ("ios-arm64-simulator", "ios", ["arm64"], "simulator"),
            ):
                entries.append({"LibraryIdentifier": identifier, "SupportedPlatform": platform,
                                "SupportedArchitectures": architectures, "SupportedPlatformVariant": variant})
                files[f"RustTidkod.xcframework/{identifier}/libtidkod_bindings.a"] = b""
            files["RustTidkod.xcframework/Info.plist"] = plistlib.dumps({"AvailableLibraries": entries})
        else:
            target = next(t for t in sdk.TARGETS if t in name)
            variant = "native" if "-native-" in name else "core"
            profile = "debug" if bad_profile else "release"
            files["BUILD.txt"] = f"version=1.2.3\ntarget={target}\nvariant={variant}\nprofile={profile}\n".encode()
            for required in ("include/tidkod.h", "include/tidkod-client.hpp", "examples/client.c", "examples/client.cpp"):
                files[required] = b""
            lib = "tidkod_bindings.dll" if "windows" in target else "libtidkod_bindings." + ("dylib" if "apple" in target else "so")
            files["lib/" + lib] = binary(target.replace("aarch64", "x86_64") if wrong_arch else target)
            files["lib/tidkod_bindings.lib" if "windows" in target else "lib/libtidkod_bindings.a"] = b""
            if "windows" in target:
                for required in ("lib/tidkod_bindings.dll.lib", "csharp/Tidkod.csproj", "include/NativeMethods.g.cs"):
                    files[required] = b""
            if "apple" in target:
                for required in ("Package.swift", "swift-dylib/libTidkod.dylib", "swift-dylib/libTidkodSys.dylib"):
                    files[required] = b""
        path = directory / name
        if name.endswith(".zip"):
            with zipfile.ZipFile(path, "w") as z:
                for relative, data in files.items():
                    z.writestr(root + "/" + relative, data)
        else:
            with tarfile.open(path, "w:gz") as t:
                for relative, data in files.items():
                    item = tarfile.TarInfo(root + "/" + relative)
                    item.size = len(data)
                    t.addfile(item, io.BytesIO(data))


class ReleaseTests(unittest.TestCase):
    def test_complete_release_has_checksums_for_every_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            fixtures(directory)
            assets = release.validate_assets(directory, "1.2.3")
            checksums = (directory / "SHA256SUMS").read_text().splitlines()
            self.assertEqual(len(assets), 12)
            self.assertEqual(len(checksums), 11)
            for line in checksums:
                digest, name = line.split("  ")
                self.assertEqual(digest, hashlib.sha256((directory / name).read_bytes()).hexdigest())

    def test_missing_target_prevents_release(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            fixtures(directory)
            next(directory.glob("*aarch64-pc-windows-msvc.zip")).unlink()
            with self.assertRaisesRegex(AssertionError, "Incomplete"):
                release.validate_assets(directory, "1.2.3")
            self.assertFalse((directory / "SHA256SUMS").exists())

    def test_debug_or_wrong_architecture_is_rejected(self):
        for options in ({"bad_profile": True}, {"wrong_arch": True}):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as temp:
                directory = Path(temp)
                fixtures(directory, **options)
                with self.assertRaises(AssertionError):
                    release.validate_assets(directory, "1.2.3")

    def test_archive_cannot_escape_extraction_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            path = directory / "bad.zip"
            with zipfile.ZipFile(path, "w") as z:
                z.writestr("../escaped", "bad")
            with self.assertRaisesRegex(RuntimeError, "Unsafe"):
                sdk.unpack(path, directory / "unpacked")
            self.assertFalse((directory / "escaped").exists())

    def test_published_release_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            asset = Path(temp) / "example.zip"
            asset.write_bytes(b"new")
            existing = [[{"tag_name": "v1.2.3", "draft": False, "assets": [
                {"name": asset.name, "digest": "sha256:old"}]}]]
            with patch.object(release, "check"), patch.object(release, "version", return_value="1.2.3"), \
                 patch.object(release, "validate_assets", return_value=[asset]), \
                 patch.object(release, "run", return_value=json.dumps(existing)) as run:
                with self.assertRaisesRegex(AssertionError, "Published asset differs"):
                    release.publish("v1.2.3", Path(temp))
                self.assertEqual(run.call_count, 1)

    def test_identical_published_release_is_a_noop(self):
        with tempfile.TemporaryDirectory() as temp:
            asset = Path(temp) / "example.zip"
            asset.write_bytes(b"same")
            existing = [[{"tag_name": "v1.2.3", "draft": False, "assets": [
                {"name": asset.name, "digest": "sha256:" + hashlib.sha256(b"same").hexdigest()}]}]]
            with patch.object(release, "check"), patch.object(release, "version", return_value="1.2.3"), \
                 patch.object(release, "validate_assets", return_value=[asset]), \
                 patch.object(release, "run", return_value=json.dumps(existing)) as run:
                release.publish("v1.2.3", Path(temp))
                self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()
