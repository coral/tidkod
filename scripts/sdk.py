#!/usr/bin/env python3
"""Build, archive, and independently exercise distributable native SDKs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
)


def run(*args, cwd=ROOT, env=None, capture=False, timeout=None):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=cwd, env=env, check=True,
                          text=True, encoding="utf-8", stdout=subprocess.PIPE if capture else None,
                          timeout=timeout).stdout


def version():
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def notices(sdk, target, variant):
    """Include license texts plus exact source links, including MPL source access."""
    features = "c,cpp,csharp" + (",swift" if "apple" in target else "")
    if variant == "native":
        features += ",native"
    metadata = json.loads(run("cargo", "metadata", "--locked", "--format-version", "1",
                              "--filter-platform", target, "--no-default-features",
                              "--features", "tidkod-bindings/" + features.replace(",", ",tidkod-bindings/"), capture=True))
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    start = next(p["id"] for p in packages.values() if p["name"] == "tidkod-bindings")
    pending, seen = [start], set()
    while pending:
        item = pending.pop()
        if item in seen:
            continue
        seen.add(item)
        pending.extend(d["pkg"] for d in nodes[item]["deps"]
                       if any(k["kind"] != "dev" for k in d["dep_kinds"]))
    inventory = []
    for ident in sorted(seen):
        p = packages[ident]
        if not p["source"]:
            continue
        if not p["license"] and not p["license_file"]:
            raise RuntimeError(f"Missing license declaration: {ident}")
        source = Path(p["manifest_path"]).parent
        suffix = hashlib.sha256(ident.encode()).hexdigest()[:8]
        destination = sdk / "licenses" / f'{p["name"]}-{p["version"]}-{suffix}'
        copied = []
        for f in source.rglob("*"):
            if f.is_file() and f.name.upper().startswith(("LICENSE", "COPYING", "NOTICE")):
                relative = f.relative_to(source)
                out = destination / relative
                out.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(f, out)
                copied.append(str(out.relative_to(sdk)))
        if p["license_file"]:
            f = source / p["license_file"]
            destination.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(f, destination / f.name)
            copied.append(str((destination / f.name).relative_to(sdk)))
        if p["source"].startswith("git+"):
            url, revision = p["source"][4:].split("#")
            source_url = url.split("?")[0] + "/tree/" + revision
            for parent in [source, *source.parents]:
                if (parent / ".git").exists():
                    for f in parent.glob("LICENSE*"):
                        destination.mkdir(parents=True, exist_ok=True)
                        shutil.copyfile(f, destination / f.name)
                        copied.append(str((destination / f.name).relative_to(sdk)))
                    break
        else:
            source_url = f'https://crates.io/api/v1/crates/{p["name"]}/{p["version"]}/download'
        inventory.append({"name": p["name"], "version": p["version"], "license": p["license"],
                          "source": source_url, "license_files": sorted(set(copied))})
    # The LTC decoder is adapted source, not a Cargo registry dependency.
    ltc_notice = sdk / "licenses" / "ltc-0.2.0-adapted" / "LICENSE.txt"
    ltc_notice.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / "protocol/licenses/ltc-BSD-3-Clause.txt", ltc_notice)
    inventory.append({"name": "ltc-adapted", "version": "0.2.0", "license": "BSD-3-Clause",
                      "source": "https://crates.io/api/v1/crates/ltc/0.2.0/download",
                      "license_files": [str(ltc_notice.relative_to(sdk))]})
    (sdk / "THIRD-PARTY.json").write_text(json.dumps(inventory, indent=2) + "\n")
    (sdk / "THIRD-PARTY.txt").write_text(
        "Dependency inventory includes native dependencies and build tools.\n"
        "Their licenses remain applicable independently of the Tidkod license.\n"
        "Exact upstream source is available at the URLs in THIRD-PARTY.json.\n"
        "MPL-2.0-covered source, including triple_buffer, is available at its listed URL\n"
        "under MPL-2.0. No modifications to those dependencies are included.\n"
        "Adapted MoQ transport source and attribution are included in source/native/.\n"
        "Adapted BSD LTC decoder source and attribution are included in source/protocol/.\n")


def archive(sdk, out, name, windows=False):
    out.mkdir(parents=True, exist_ok=True)
    # Archive from a versioned top-level directory so extraction is unambiguous.
    root = sdk.parent / name
    sdk.rename(root)
    if windows:
        return Path(shutil.make_archive(str(out / name), "zip", root.parent, root.name))
    return Path(shutil.make_archive(str(out / name), "gztar", root.parent, root.name))


def unpack(path, destination):
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as z:
            for item in z.namelist():
                if not (destination / item).resolve().is_relative_to(destination.resolve()):
                    raise RuntimeError("Unsafe archive member")
            z.extractall(destination)
    else:
        with tarfile.open(path) as t:
            t.extractall(destination, filter="data")
    roots = list(destination.iterdir())
    if len(roots) != 1 or not roots[0].is_dir():
        raise RuntimeError("SDK archive must contain one root directory")
    return roots[0]


def machine(path):
    data = path.read_bytes()
    if data[:2] == b"MZ":
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        return {0x8664: "x86_64", 0xAA64: "aarch64"}[struct.unpack_from("<H", data, offset + 4)[0]]
    if data[:4] == b"\x7fELF":
        return {62: "x86_64", 183: "aarch64"}[struct.unpack_from("<H", data, 18)[0]]
    if data[:4] == b"\xcf\xfa\xed\xfe":
        return {0x1000007: "x86_64", 0x100000C: "aarch64"}[struct.unpack_from("<I", data, 4)[0]]
    raise RuntimeError(f"Unrecognized binary format: {path}")


def test_sdk(path):
    with tempfile.TemporaryDirectory(prefix="tidkod-consumer-") as temporary:
        work = Path(temporary)
        sdk = unpack(path.resolve(), work / "unpacked")
        info = dict(line.split("=", 1) for line in (sdk / "BUILD.txt").read_text().splitlines())
        target, variant = info["target"], info["variant"]
        library = "tidkod_bindings.dll" if "windows" in target else "libtidkod_bindings." + ("dylib" if "apple" in target else "so")
        assert machine(sdk / "lib" / library) == target.split("-")[0]
        for name in ["LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY.json", "include/tidkod.h", "include/tidkod-client.hpp"]:
            assert (sdk / name).is_file(), name
        assert info["version"] in sdk.name
        env = os.environ.copy()
        for key in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"):
            env.pop(key, None)
        build = work / "consumer"
        options = []
        if "windows" in target:
            options = ["-A", "ARM64" if target.startswith("aarch64") else "x64"]
        run("cmake", "-S", sdk, "-B", build, "-DCMAKE_BUILD_TYPE=Release", *options, env=env)
        run("cmake", "--build", build, "--config", "Release", env=env)
        run("ctest", "--test-dir", build, "-C", "Release", "--output-on-failure", env=env)
        executable = build / ("Release/tidkod-shared-client-c.exe" if "windows" in target else "tidkod-shared-client-c")
        assert machine(executable) == target.split("-")[0]
        if "windows" in target:
            dependencies = run("dumpbin", "/dependents", executable, capture=True, env=env)
        elif "apple" in target:
            dependencies = run("otool", "-L", executable, capture=True, env=env)
        else:
            dependencies = run("ldd", executable, capture=True, env=env)
            assert "not found" not in dependencies
            assert str(build) in dependencies
        assert library in dependencies, dependencies
        if "windows" in target:
            run("dotnet", "run", "--project", sdk / "csharp/Smoke", "-c", "Release",
                "--arch", "arm64" if target.startswith("aarch64") else "x64", env=env, timeout=180)
        if "apple" in target:
            run("swift", "run", "--package-path", sdk, "-c", "release", "TidkodSmoke", env=env, timeout=180)
            run(sdk / "swift-dylib/TidkodSmoke", cwd=work, env=env, timeout=30)
            if variant == "native":
                # Compile the shipped adapter with its private OS-boundary tests.
                # The optional real multicast tests use the very same executable.
                swift_test = work / "BonjourTests.swift"
                swift_test.write_text((sdk / "swift-client/Tidkod.swift").read_text() + "\n" +
                                      (sdk / "source/clients/bindings/tests/bonjour.swift").read_text())
                run("swiftc", "-O", "-parse-as-library", swift_test,
                    "-I", sdk / "swift-dylib", "-I", sdk / "swift-c",
                    "-L", sdk / "swift-dylib", "-lTidkodSys",
                    "-Xlinker", "-rpath", "-Xlinker", sdk / "swift-dylib",
                    "-Xlinker", "-rpath", "-Xlinker", sdk / "lib",
                    "-o", work / "BonjourTests", env=env, timeout=180)
                run(work / "BonjourTests", cwd=work, env=env, timeout=120)
        # The extracted sources must be independently usable, with the supplied lockfile.
        run("cargo", "check", "--manifest-path", sdk / "source/Cargo.toml", "-p", "tidkod-bindings",
            "--locked", "--no-default-features", "--features", "c,cpp,csharp" + (",native" if variant == "native" else ""),
            "--target-dir", ROOT / "target/sdk-source-check", env=env)


def build_sdk(args):
    target, variant = args.target, args.variant
    features = "c,cpp,csharp" + (",swift" if "apple" in target else "") + (",native" if variant == "native" else "")
    artifacts = ROOT / "target" / ("sdk-" + variant)
    run("cargo", "build", "-p", "tidkod-bindings", "--locked", "--target", target,
        "--target-dir", artifacts, "--no-default-features", "--features", features,
        *(["--release"] if args.profile == "release" else []))
    with tempfile.TemporaryDirectory(prefix="tidkod-package-") as temporary:
        env = os.environ.copy()
        env.update(TIDKOD_SDK_ARTIFACTS=str(artifacts / target / args.profile),
                   TIDKOD_SDK_OUT=temporary, TIDKOD_SDK_VARIANT=variant)
        if "apple" in target:
            env["TIDKOD_SDK_SWIFT_DYLIB"] = "1"
        run("cargo", "build", "-p", "tidkod-sdk", "--locked", env=env)
        sdk = Path(temporary) / f"tidkod-{variant}-{target}"
        with (sdk / "BUILD.txt").open("a") as out:
            out.write(f"profile={args.profile}\nrevision={run('git', 'rev-parse', 'HEAD', capture=True).strip()}\n")
        notices(sdk, target, variant)
        result = archive(sdk, args.out.resolve(), f"tidkod-{version()}-{variant}-{target}", "windows" in target)
    test_sdk(result)
    print(f"Verified SDK: {result}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("build")
    build.add_argument("--target", choices=TARGETS, required=True)
    build.add_argument("--variant", choices=("native", "core"), required=True)
    build.add_argument("--profile", choices=("debug", "release"), default="release")
    build.add_argument("--out", type=Path, default=ROOT / "dist/releases")
    test = commands.add_parser("test")
    test.add_argument("archive", type=Path)
    args = parser.parse_args()
    if args.command == "build":
        build_sdk(args)
    else:
        test_sdk(args.archive)


if __name__ == "__main__":
    main()
