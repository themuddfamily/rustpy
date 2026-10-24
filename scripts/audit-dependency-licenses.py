#!/usr/bin/env python3
"""Inventory locked Cargo license declarations; does not certify compliance.

Run from the repository root with Python 3.11+ and cached Cargo sources.
No compilation or dependency updates. Output includes all targets/features and
the separate QA/fuzz locks. License file discovery is evidence, not validation.
"""

import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[1]
LOCKS = ("Cargo.lock", "qa/Cargo.lock", "qa/fuzz/Cargo.lock")


def metadata(*args):
    return json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1", *args],
        cwd=ROOT,
    ))


def assessment(expression, name):
    if not expression:
        return "REVIEW: missing declaration"
    if expression == "MPL-2.0":
        return "File copyleft: retain MPL notices; provide covered source and source-location notice for binaries"
    if name == "epaint_default_fonts":
        return "Combined code/font terms: retain all font notices; observe font naming and redistribution conditions"
    if name == "libfuzzer-sys":
        return "QA only: MIT or Apache plus NCSA; retain runtime license notices"
    if " AND " in expression:
        return "Combined obligations: comply with each AND branch; choose an allowed alternative within OR branches"
    if expression == "CDLA-Permissive-2.0":
        return "Permissive data: ship agreement with certificate data"
    if expression == "Unicode-3.0":
        return "Permissive: preserve Unicode copyright and permission notice"
    if expression == "Apache-2.0 WITH LLVM-exception":
        return "Apache plus LLVM exception: preserve license, exception, notices and applicable change notices"
    if " OR " in expression or "/" in expression:
        return "Alternative licenses: permissive route available; document selection and retain applicable notices"
    if expression in {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "BSL-1.0", "MIT-0", "CC0-1.0"}:
        return "Permissive: retain applicable license/copyright notices and conditions"
    return "REVIEW: unclassified expression"


def main():
    default = metadata()
    complete = metadata("--all-features")
    default_ids = {(p["name"], p["version"], p.get("source") or "path") for p in default["packages"]}
    meta = {(p["name"], p["version"], p.get("source") or "path"): p for p in complete["packages"]}
    workspace = set(complete["workspace_members"])
    direct_ids = {
        d["pkg"] for node in complete["resolve"]["nodes"] if node["id"] in workspace
        for d in node["deps"] if d["pkg"] not in workspace
    }
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    cached = {}
    for base in sorted((cargo_home / "registry/src").glob("*")):
        for directory in base.iterdir():
            if (directory / "Cargo.toml").exists():
                cached.setdefault(directory.name, []).append(directory)
    records = {}
    overrides_path = ROOT / "packaging/cargo-license-overrides.json"
    overrides = json.loads(overrides_path.read_text())["packages"] if overrides_path.exists() else {}
    for lock in LOCKS:
        for package in tomllib.loads((ROOT / lock).read_text())["package"]:
            key = (package["name"], package["version"], package.get("source") or "path")
            row = records.setdefault(key, {"locks": [], "checksum": package.get("checksum", "")})
            row["locks"].append(lock)

    rows = []
    for (name, version, source), record in sorted(records.items()):
        key = (name, version, source)
        p = meta.get(key)
        directory = Path(p["manifest_path"]).parent if p else None
        if directory is None and source.startswith("registry+"):
            # Cargo's ordinary registry extraction may omit .cargo-checksum.json.
            # Authenticate the cached crate archive against the locked checksum.
            for candidate in cached.get(f"{name}-{version}", []):
                check = candidate / ".cargo-checksum.json"
                archive = cargo_home / "registry/cache" / candidate.parent.name / f"{name}-{version}.crate"
                matched = check.exists() and json.loads(check.read_text()).get("package") == record["checksum"]
                if not matched and archive.exists():
                    matched = hashlib.sha256(archive.read_bytes()).hexdigest() == record["checksum"]
                if matched:
                    directory = candidate
                    break
        if directory is None and source == "path":
            directory = ROOT / {"rustpy-quality": "qa", "rustpy-fuzz": "qa/fuzz"}.get(name, "__missing__")
        manifest = directory / "Cargo.toml" if directory else None
        data = tomllib.loads(manifest.read_text())["package"] if manifest and manifest.exists() else {}
        license_value = p.get("license") if p else data.get("license")
        if isinstance(license_value, dict):
            license_value = "Apache-2.0" if license_value.get("workspace") else None
        own = bool(p and p["id"] in workspace) or name in {"rustpy-quality", "rustpy-fuzz"}
        files = []
        if directory and directory.exists():
            for child in directory.rglob("*"):
                if child.is_file() and any(child.name.lower().startswith(prefix) for prefix in ("license", "licence", "copying", "notice", "copyright")):
                    files.append(child.relative_to(directory).as_posix())
        scope = "workspace" if own else "direct" if p and p["id"] in direct_ids else "transitive" if p else "QA/fuzz-only"
        rows.append({
            "name": name, "version": version, "scope": scope,
            "lockfiles": "; ".join(record["locks"]),
            "in_default_metadata": str(key in default_ids).lower(),
            "in_all_features_metadata": str(p is not None).lower(),
            "declared_license": license_value or "UNDECLARED",
            "assessment": "Project license declaration; see assessment report" if own else assessment(license_value, name),
            "source": source, "checksum": record["checksum"],
            "repository": (p or data).get("repository") or "",
            "license_file_declaration": ((p or {}).get("license_file") or data.get("license-file") or ""),
            "license_notice_files": "; ".join(sorted(files)),
            "retained_upstream_supplements": "; ".join(
                f["path"] for f in overrides.get(f"{name}@{version}", {}).get("files", [])
            ),
        })
    destination = ROOT / "docs/dependency-licenses.csv"
    with destination.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=list(rows[0]), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
    print(f"Wrote {len(rows)} package/version/source records to {destination.relative_to(ROOT)}")
    for lock in LOCKS:
        print(f"{lock}: sha256 {hashlib.sha256((ROOT / lock).read_bytes()).hexdigest()}")
    print(f"All-features external packages: {len(complete['packages']) - len(workspace)}")
    print("Metadata includes development/build/target-specific packages; not a binary SBOM.")


if __name__ == "__main__":
    main()
