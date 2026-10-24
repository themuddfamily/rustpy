#!/usr/bin/env python3
"""Retain locked Cargo license texts and source locations, without compilation.

Generation needs cached .crate archives or --cargo-vendor. --check only needs
committed inputs. --source-bundle additionally exports covered source archives.
"""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
LOCKS = ("Cargo.lock", "qa/Cargo.lock", "qa/fuzz/Cargo.lock")
NOTICE = "packaging/CARGO-THIRD-PARTY-NOTICES.txt"
MANIFEST = "packaging/cargo-license-manifest.json"
OVERRIDES = "packaging/cargo-license-overrides.json"
# Include exact corresponding source alongside binaries, not just a URL. The
# GPL exception in libgit2 does not erase its covered-source obligations.
COVERED = {"cssparser", "cssparser-macros", "dtoa-short", "option-ext",
           "selectors", "version-ranges", "libgit2-sys", "wayland-protocols-plasma"}
PERMISSIVE = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC",
              "Zlib", "Unicode-3.0", "Unlicense", "CC0-1.0", "BSL-1.0",
              "0BSD", "MIT-0", "BSD-1-Clause", "CDLA-Permissive-2.0",
              "OFL-1.1", "Ubuntu-font-1.0", "NCSA", "MPL-2.0",
              "Apache-2.0 WITH LLVM-exception"}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path):
    return json.loads((ROOT / path).read_text())


def locked_packages():
    packages = {}
    for lock in LOCKS:
        for p in tomllib.loads((ROOT / lock).read_text())["package"]:
            if p.get("source"):
                if p["source"] != "registry+https://github.com/rust-lang/crates.io-index":
                    raise ValueError(f"Unreviewed source: {p['source']}")
                key = f"{p['name']}@{p['version']}"
                prior = packages.setdefault(key, {**p, "locks": []})
                if prior["checksum"] != p["checksum"]:
                    raise ValueError(f"Conflicting source checksums for {key}")
                prior["locks"].append(lock)
    return dict(sorted(packages.items()))


def selected_terms(expression):
    """Select an allowed OR branch while retaining all AND obligations."""
    tokens = re.findall(r"\(|\)|[^\s()]+", expression.replace("/", " OR "))
    position = 0

    def atom():
        nonlocal position
        if tokens[position] == "(":
            position += 1
            value = either()
            if tokens[position] != ")":
                raise ValueError(expression)
            position += 1
            return value
        term = tokens[position]
        position += 1
        if position < len(tokens) and tokens[position] == "WITH":
            term += " WITH " + tokens[position + 1]
            position += 2
        return {term}

    def both():
        nonlocal position
        value = atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            value |= atom()
        return value

    def either():
        nonlocal position
        choices = [both()]
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            choices.append(both())
        allowed = [c for c in choices if c <= PERMISSIVE]
        if not allowed:
            raise ValueError(f"No reviewed license route: {expression}")
        return min(allowed, key=lambda c: (sum(0 if t == "MIT" else 1 if t == "Apache-2.0" else 2 for t in c), sorted(c)))

    result = either()
    if position != len(tokens):
        raise ValueError(f"Unparsed license: {expression}")
    return sorted(result)


def named_notice(path):
    return any(Path(path).name.lower().startswith(p) for p in
               ("license", "licence", "copying", "notice", "copyright"))


class Sources:
    def __init__(self, vendor=None):
        self.cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
        self.vendor = Path(vendor) if vendor else ROOT / "cargo/vendor"

    def archive(self, package):
        name = f"{package['name']}-{package['version']}.crate"
        candidates = list((self.cargo / "registry/cache").glob(f"*/{name}"))
        extra = os.environ.get("RUSTPY_NATIVE_CRATE_CACHE")
        if extra:
            candidates.extend(Path(extra).rglob(name))
        for archive in sorted(set(candidates)):
            data = archive.read_bytes()
            if digest(data) == package["checksum"]:
                return data
        return None

    def files(self, package):
        archive = self.archive(package)
        if archive is not None:
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
                files = {}
                for member in tar.getmembers():
                    if member.isfile():
                        relative = member.name.split("/", 1)[1]
                        if relative == "Cargo.toml" or named_notice(relative):
                            files[relative] = tar.extractfile(member).read()
                return files
        raise ValueError(f"Missing authenticated archive for {package['name']} {package['version']}; cargo fetch --locked, or provide RUSTPY_NATIVE_CRATE_CACHE")

    def verify_vendor(self, package, archive):
        if not self.vendor.is_dir():
            return
        for name in (f"{package['name']}-{package['version']}", package['name']):
            directory = self.vendor / name
            check = directory / ".cargo-checksum.json"
            if not check.exists():
                continue
            checks = json.loads(check.read_text())
            if checks.get("package") != package["checksum"]:
                continue
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
                original = {m.name.split("/", 1)[1]: digest(tar.extractfile(m).read())
                            for m in tar.getmembers() if m.isfile()}
            if checks["files"] != original:
                raise ValueError(f"Vendor inventory differs from authenticated archive: {directory}")
            for relative, expected in sorted(original.items()):
                if digest((directory / relative).read_bytes()) != expected:
                    raise ValueError(f"Modified covered source: {directory / relative}; retain reviewed modifications and corresponding source")
            actual = {p.relative_to(directory).as_posix() for p in directory.rglob("*")
                      if p.is_file() and p.name != ".cargo-checksum.json"}
            if actual != set(original):
                raise ValueError(f"Vendor files differ from authenticated archive: {directory}")
            return
        raise ValueError(f"Missing covered source in vendor directory: {package['name']} {package['version']}")


def generate(sources):
    overrides = read_json(OVERRIDES)["packages"]
    texts = {}
    records = []
    inputs = {p: digest((ROOT / p).read_bytes()) for p in (*LOCKS, OVERRIDES, 'scripts/generate-cargo-notices.py')}
    for key, package in locked_packages().items():
        files = sources.files(package)
        meta = tomllib.loads(files.pop("Cargo.toml").decode("utf-8"))["package"]
        expression = meta.get("license")
        if not expression:
            raise ValueError(f"Missing SPDX declaration for {key}")
        references = []
        for path, data in sorted(files.items()):
            # Keep every nested copyright/license file, even for alternative
            # licenses not selected. Retaining text is not electing that route.
            text = data.decode("utf-8-sig")
            identifier = digest(text.encode())
            texts[identifier] = text
            references.append({"path": path, "text_sha256": identifier})
        for extra in overrides.get(key, {}).get("files", []):
            path = extra["path"]
            data = (ROOT / path).read_bytes()
            if digest(data) != extra["sha256"]:
                raise ValueError(f"Override checksum mismatch: {path}")
            inputs[path] = digest(data)
            text = data.decode("utf-8-sig")
            identifier = digest(text.encode())
            texts[identifier] = text
            references.append({"path": extra.get("url", path), "text_sha256": identifier})
        if not references:
            raise ValueError(f"No retained license text for {key}; add a pinned upstream override")
        records.append({"package": key, "license": expression,
                        "selected_terms": selected_terms(expression),
                        "source_url": f"https://crates.io/api/v1/crates/{package['name']}/{package['version']}/download",
                        "source_sha256": package["checksum"], "lockfiles": package["locks"],
                        "covered_source_bundled": package["name"] in COVERED,
                        "texts": references})
    manifest = {"format": 1, "input_sha256": dict(sorted(inputs.items())), "packages": records}
    rendered = ["RustPy — locked Cargo dependency licenses and source locations\n",
                "This conservative inventory covers all three lockfiles, all targets, optional\n"
                "and development dependencies; not every listed component is in every binary.\n"
                "Selected terms record the license route used for the package declaration.\n"
                "All discovered upstream license/notice texts are retained, including alternatives\n"
                "and nested native-component terms. The resource/native supplements also apply.\n"
                "A wrapper's selected terms do not override licenses of embedded native code.\n\n"
                "Covered source: Native-Licenses.zip contains cargo-covered-sources.tar.gz\n"
                "with the exact MPL package and selected native covered source. Extract the zip\n"
                "and read its build/source instructions. Each package below also has an exact\n"
                "public source archive URL. Covered MPL sources and modifications remain MPL.\n"
                "The application and its separate source files are licensed independently.\n"]
    for record in records:
        rendered.append("\n" + record["package"] + "\n" + "-" * 72 + "\n"
                        + "Declared: " + record["license"] + "\n"
                        + "Selected: " + " AND ".join(record["selected_terms"]) + "\n"
                        + "Source: " + record["source_url"] + "\n"
                        + "Source SHA-256: " + record["source_sha256"] + "\n")
        for ref in record["texts"]:
            rendered.append("Notice: " + ref["path"] + "\nText SHA-256: " + ref["text_sha256"] + "\n")
    rendered.append("\n\nVERBATIM UPSTREAM TEXTS (deduplicated by SHA-256)\n")
    for identifier, text in sorted(texts.items()):
        rendered.append("\nText SHA-256: " + identifier + "\n" + "=" * 72 + "\n" + text + ("" if text.endswith("\n") else "\n"))
    notice = "".join(rendered).encode()
    manifest["notice_sha256"] = digest(notice)
    return notice, manifest


def check():
    manifest = read_json(MANIFEST)
    for path, expected in manifest["input_sha256"].items():
        if digest((ROOT / path).read_bytes()) != expected:
            raise ValueError(f"Cargo notices stale: {path} changed")
    if digest((ROOT / NOTICE).read_bytes()) != manifest["notice_sha256"]:
        raise ValueError("Cargo notice text changed; regenerate")
    expected = locked_packages()
    if {r["package"] for r in manifest["packages"]} != set(expected):
        raise ValueError("Cargo notice package inventory incomplete")
    for record in manifest["packages"]:
        if record["source_sha256"] != expected[record["package"]]["checksum"] or not record["texts"]:
            raise ValueError(f"Missing/mismatched license evidence: {record['package']}")
        if record["selected_terms"] != selected_terms(record["license"]):
            raise ValueError(f"Invalid selected terms: {record['package']}")
    return manifest


def source_bundle(sources, output):
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    instructions = ("RustPy covered Cargo source\n==========================\n\n"
                    "These are the exact upstream sources for the covered components, including\n"
                    "their original license terms and build scripts. No upstream registry source\n"
                    "changes were made by RustPy. Other dependencies are listed with exact source\n"
                    "URLs/checksums in THIRD-PARTY-NOTICES.txt accompanying the executable.\n\n"
                    "Unpack .crate files with tar (they are gzip tar archives). Directory entries\n"
                    "are Cargo vendor sources with file checksum manifests. The original RustPy\n"
                    "release source, Cargo.lock, vendor patches and packaging recipes are the\n"
                    "build inputs: obtain that same release revision from the source distribution\n"
                    "or https://github.com/themuddfamily/rustpy. Use its documented Rust toolchain and\n"
                    "platform native prerequisites; run cargo build --locked --release -p rustpy\n"
                    "(add --features native-web-preview for that feature). These sources may be\n"
                    "used to replace the matching registry cache/vendor packages. Do not change\n"
                    "the license of covered MPL files; retain all upstream notices.\n")
    # Atomic output: a missing source must never leave an apparently usable bundle.
    temporary = output.with_name(output.name + ".partial")
    try:
        with temporary.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
            def add(name, data):
                info = tarfile.TarInfo(name)
                info.size, info.mode, info.mtime = len(data), 0o644, 0
                tar.addfile(info, io.BytesIO(data))
            add("README.txt", instructions.encode())
            for key, package in locked_packages().items():
                if package["name"] not in COVERED:
                    continue
                archive = sources.archive(package)
                if archive is not None:
                    sources.verify_vendor(package, archive)
                    add(f"{package['name']}-{package['version']}.crate", archive)
                    continue
                raise ValueError(f"Missing authenticated covered source: {key}; cargo fetch --locked or provide RUSTPY_NATIVE_CRATE_CACHE")
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--verify-cache", action="store_true", help="also reproduce all retained Cargo text from cached sources")
    parser.add_argument("--cargo-vendor", type=Path)
    parser.add_argument("--source-bundle", type=Path)
    args = parser.parse_args()
    sources = Sources(args.cargo_vendor)
    try:
        if args.check:
            current = check()
            if args.verify_cache:
                notice, manifest = generate(sources)
                if notice != (ROOT / NOTICE).read_bytes() or manifest != current:
                    raise ValueError("Cached source reproduction differs from retained Cargo notices")
        else:
            notice, manifest = generate(sources)
            (ROOT / NOTICE).write_bytes(notice)
            (ROOT / MANIFEST).write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
        if args.source_bundle:
            source_bundle(sources, args.source_bundle)
        print("Cargo license evidence: OK")
    except (ValueError, OSError, KeyError, IndexError) as error:
        parser.exit(1, f"Cargo license evidence error: {error}\n")


if __name__ == "__main__":
    main()
