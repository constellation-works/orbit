#!/usr/bin/env python3
"""Fail when vendored dashboard JS drifts from its recorded pins.

The dashboard self-hosts DOMPurify and marked as checked-in blobs. Pins,
upstream URLs, and SHA-256 digests live in vendor/vendor-manifest.json; npm
identity for Dependabot lives in package.json and package-lock.json at the
dashboard root. This
check is the regression gate: an undocumented swap, a version bump that does
not refresh the copies, or a missing or drifted lockfile cannot land silently.

Runs from `scripts/ci-guardrails.sh` (and `make ci-fast`).
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import tempfile
from argparse import ArgumentParser
from pathlib import Path
from typing import Any


PACKAGE_DIR = Path("crates/orbit-web/assets/dashboard")
VENDOR_DIR = PACKAGE_DIR / "vendor"
MANIFEST_NAME = "vendor-manifest.json"
PACKAGE_NAME = "package.json"
LOCKFILE_NAME = "package-lock.json"
REQUIRED_ASSET_FIELDS = (
    "name",
    "npm_package",
    "version",
    "upstream",
    "source",
    "npm_file",
    "path",
    "sha256",
)
EXACT_VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path, label: str) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise RuntimeError(f"missing {label} at {path}") from None
    except json.JSONDecodeError as error:
        raise RuntimeError(f"invalid JSON in {label} {path}: {error}") from error


def check_vendor(root: Path) -> list[str]:
    """Return human-readable failures for one repository root."""
    vendor = root / VENDOR_DIR
    manifest_path = vendor / MANIFEST_NAME
    package_path = root / PACKAGE_DIR / PACKAGE_NAME
    lockfile_path = root / PACKAGE_DIR / LOCKFILE_NAME
    manifest_label = str(VENDOR_DIR / MANIFEST_NAME)
    package_label = str(PACKAGE_DIR / PACKAGE_NAME)
    lockfile_label = str(PACKAGE_DIR / LOCKFILE_NAME)
    failures: list[str] = []

    manifest = load_json(manifest_path, "vendor manifest")
    package = load_json(package_path, "package.json")

    if not isinstance(manifest, dict):
        return [f"{manifest_label}: manifest must be a JSON object"]
    if manifest.get("schema_version") != 1:
        failures.append(
            f"{manifest_label}: schema_version must be 1, got {manifest.get('schema_version')!r}"
        )
    refresh = manifest.get("refresh")
    if not isinstance(refresh, str) or not refresh.strip():
        failures.append(f"{manifest_label}: refresh command must be a non-empty string")

    assets = manifest.get("assets")
    if not isinstance(assets, list) or not assets:
        failures.append(f"{manifest_label}: assets must be a non-empty array")
        return failures

    if not isinstance(package, dict):
        return failures + [f"{package_label}: package.json must be a JSON object"]
    dependencies = package.get("dependencies")
    if not isinstance(dependencies, dict) or not dependencies:
        failures.append(f"{package_label}: dependencies must be a non-empty object")
        dependencies = {}

    lock_packages: dict[str, Any] = {}
    lock_dependencies: dict[str, Any] = {}
    has_lockfile = False
    if not lockfile_path.is_file():
        failures.append(f"{lockfile_label}: missing")
    else:
        lockfile = load_json(lockfile_path, "package-lock.json")
        if not isinstance(lockfile, dict):
            failures.append(f"{lockfile_label}: package-lock.json must be a JSON object")
        else:
            has_lockfile = True
            packages_field = lockfile.get("packages")
            if not isinstance(packages_field, dict):
                failures.append(f"{lockfile_label}: packages must be an object")
            else:
                lock_packages = packages_field
                root_entry = packages_field.get("")
                if not isinstance(root_entry, dict):
                    failures.append(f'{lockfile_label}: packages[""] must be an object')
                else:
                    root_deps = root_entry.get("dependencies")
                    if not isinstance(root_deps, dict) or not root_deps:
                        failures.append(
                            f'{lockfile_label}: packages[""].dependencies must be a non-empty object'
                        )
                    else:
                        lock_dependencies = root_deps

    seen_packages: set[str] = set()
    seen_paths: set[str] = set()

    for index, asset in enumerate(assets):
        prefix = f"{manifest_label}: assets[{index}]"
        if not isinstance(asset, dict):
            failures.append(f"{prefix}: must be an object")
            continue
        missing = [field for field in REQUIRED_ASSET_FIELDS if field not in asset]
        if missing:
            failures.append(f"{prefix}: missing fields {', '.join(missing)}")
            continue

        name = asset["name"]
        npm_package = asset["npm_package"]
        version = asset["version"]
        upstream = asset["upstream"]
        source = asset["source"]
        npm_file = asset["npm_file"]
        rel_path = asset["path"]
        recorded = asset["sha256"]

        for field_name, value in (
            ("name", name),
            ("npm_package", npm_package),
            ("version", version),
            ("upstream", upstream),
            ("source", source),
            ("npm_file", npm_file),
            ("path", rel_path),
            ("sha256", recorded),
        ):
            if not isinstance(value, str) or not value.strip():
                failures.append(f"{prefix}: {field_name} must be a non-empty string")

        if not isinstance(rel_path, str):
            continue
        if rel_path != Path(rel_path).name or rel_path in {".", ".."} or "/" in rel_path or "\\" in rel_path:
            failures.append(f"{prefix}: path must be a basename inside {VENDOR_DIR}, got {rel_path!r}")
            continue
        if rel_path in seen_paths:
            failures.append(f"{prefix}: duplicate path {rel_path}")
        seen_paths.add(rel_path)

        if isinstance(npm_package, str):
            if npm_package in seen_packages:
                failures.append(f"{prefix}: duplicate npm_package {npm_package}")
            seen_packages.add(npm_package)

        if isinstance(version, str) and not EXACT_VERSION.fullmatch(version):
            failures.append(f"{prefix}: version must be an exact semver, got {version!r}")

        if isinstance(recorded, str) and not SHA256_HEX.fullmatch(recorded):
            failures.append(f"{prefix}: sha256 must be 64 lowercase hex characters")

        blob = vendor / rel_path
        if not blob.is_file():
            failures.append(f"{prefix}: missing vendored file {VENDOR_DIR / rel_path}")
            continue
        actual = sha256_file(blob)
        if isinstance(recorded, str) and actual != recorded:
            failures.append(
                f"{VENDOR_DIR / rel_path}: sha256 mismatch for {name} {version} "
                f"(recorded {recorded}, actual {actual})"
            )

        pinned = dependencies.get(npm_package)
        if pinned is None:
            failures.append(
                f"{package_label}: missing dependency {npm_package} (manifest version {version})"
            )
        elif pinned != version:
            failures.append(
                f"{package_label}: {npm_package} is {pinned!r}, manifest records {version!r}"
            )

        if has_lockfile:
            lock_pinned = lock_dependencies.get(npm_package)
            if lock_pinned is None:
                failures.append(
                    f"{lockfile_label}: missing dependency {npm_package} (manifest version {version})"
                )
            elif lock_pinned != version:
                failures.append(
                    f"{lockfile_label}: {npm_package} is {lock_pinned!r}, manifest records {version!r}"
                )

            nm_key = f"node_modules/{npm_package}"
            nm_entry = lock_packages.get(nm_key)
            if not isinstance(nm_entry, dict):
                failures.append(f"{lockfile_label}: missing {nm_key} entry")
            else:
                resolved = nm_entry.get("version")
                if resolved != version:
                    failures.append(
                        f"{lockfile_label}: {nm_key} is {resolved!r}, manifest records {version!r}"
                    )

    extra = sorted(set(dependencies) - seen_packages)
    for npm_package in extra:
        failures.append(
            f"{package_label}: dependency {npm_package} is not recorded in {MANIFEST_NAME}"
        )

    if has_lockfile:
        extra_lock = sorted(set(lock_dependencies) - set(dependencies))
        for npm_package in extra_lock:
            failures.append(
                f"{lockfile_label}: dependency {npm_package} is not recorded in {PACKAGE_NAME}"
            )
        missing_lock = sorted(set(dependencies) - set(lock_dependencies))
        for npm_package in missing_lock:
            failures.append(
                f"{lockfile_label}: missing dependency {npm_package} from {PACKAGE_NAME}"
            )

    return failures


def write_fixture(root: Path, *, digest: str | None = None, package_version: str = "3.4.8") -> Path:
    vendor = root / VENDOR_DIR
    vendor.mkdir(parents=True)
    blob = vendor / "purify.min.js"
    blob.write_bytes(b"fixture-dompurify\n")
    recorded = digest if digest is not None else sha256_file(blob)
    (vendor / "marked.umd.js").write_bytes(b"fixture-marked\n")
    marked_digest = sha256_file(vendor / "marked.umd.js")
    manifest = {
        "schema_version": 1,
        "refresh": "./scripts/refresh-dashboard-vendor.sh",
        "assets": [
            {
                "name": "DOMPurify",
                "npm_package": "dompurify",
                "version": "3.4.8",
                "upstream": "https://github.com/cure53/DOMPurify",
                "source": "https://registry.npmjs.org/dompurify/-/dompurify-3.4.8.tgz",
                "npm_file": "dist/purify.min.js",
                "path": "purify.min.js",
                "sha256": recorded,
            },
            {
                "name": "marked",
                "npm_package": "marked",
                "version": "18.0.5",
                "upstream": "https://github.com/markedjs/marked",
                "source": "https://registry.npmjs.org/marked/-/marked-18.0.5.tgz",
                "npm_file": "lib/marked.umd.js",
                "path": "marked.umd.js",
                "sha256": marked_digest,
            },
        ],
    }
    (vendor / MANIFEST_NAME).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    package = {
        "name": "orbit-dashboard-vendor",
        "private": True,
        "dependencies": {"dompurify": package_version, "marked": "18.0.5"},
    }
    (root / PACKAGE_DIR / PACKAGE_NAME).write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")
    lockfile = {
        "name": "orbit-dashboard-vendor",
        "lockfileVersion": 3,
        "requires": True,
        "packages": {
            "": {
                "name": "orbit-dashboard-vendor",
                "dependencies": {"dompurify": package_version, "marked": "18.0.5"},
            },
            "node_modules/dompurify": {"version": package_version},
            "node_modules/marked": {"version": "18.0.5"},
        },
    }
    (root / PACKAGE_DIR / LOCKFILE_NAME).write_text(json.dumps(lockfile, indent=2) + "\n", encoding="utf-8")
    return blob


def verify_fixture_reporting() -> None:
    """Prove a matching tree passes and swapped, drifted, or missing pins fail."""
    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "ok"
        write_fixture(root)
        failures = check_vendor(root)
        if failures:
            raise RuntimeError(f"matching fixture must pass, got {failures!r}")

    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "swap"
        blob = write_fixture(root)
        recorded = sha256_file(blob)
        blob.write_bytes(b"undocumented-swap\n")
        actual = sha256_file(blob)
        failures = check_vendor(root)
        expected = (
            f"{VENDOR_DIR / 'purify.min.js'}: sha256 mismatch for DOMPurify 3.4.8 "
            f"(recorded {recorded}, actual {actual})"
        )
        if expected not in failures:
            raise RuntimeError(
                "swapped-blob fixture did not report digest mismatch: "
                f"expected {expected!r} in {failures!r}"
            )

    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "drift"
        write_fixture(root, package_version="9.9.9")
        failures = check_vendor(root)
        expected = (
            f"{PACKAGE_DIR / PACKAGE_NAME}: dompurify is '9.9.9', manifest records '3.4.8'"
        )
        if expected not in failures:
            raise RuntimeError(
                "version-drift fixture did not report pin mismatch: "
                f"expected {expected!r} in {failures!r}"
            )

    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "lock-missing"
        write_fixture(root)
        (root / PACKAGE_DIR / LOCKFILE_NAME).unlink()
        failures = check_vendor(root)
        expected = f"{PACKAGE_DIR / LOCKFILE_NAME}: missing"
        if expected not in failures:
            raise RuntimeError(
                "deleted-lockfile fixture did not report missing lockfile: "
                f"expected {expected!r} in {failures!r}"
            )

    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "lock-drift"
        write_fixture(root)
        lockfile_path = root / PACKAGE_DIR / LOCKFILE_NAME
        lockfile = json.loads(lockfile_path.read_text(encoding="utf-8"))
        lockfile["packages"][""]["dependencies"]["dompurify"] = "1.0.0"
        lockfile["packages"]["node_modules/dompurify"]["version"] = "1.0.0"
        lockfile_path.write_text(json.dumps(lockfile, indent=2) + "\n", encoding="utf-8")
        failures = check_vendor(root)
        expected_root = (
            f"{PACKAGE_DIR / LOCKFILE_NAME}: dompurify is '1.0.0', manifest records '3.4.8'"
        )
        expected_resolved = (
            f"{PACKAGE_DIR / LOCKFILE_NAME}: node_modules/dompurify is '1.0.0', "
            "manifest records '3.4.8'"
        )
        if expected_root not in failures or expected_resolved not in failures:
            raise RuntimeError(
                "drifted-lockfile fixture did not report version mismatch: "
                f"expected {expected_root!r} and {expected_resolved!r} in {failures!r}"
            )

    with tempfile.TemporaryDirectory(prefix="orbit-dashboard-vendor-") as temporary:
        root = Path(temporary) / "lock-extra"
        write_fixture(root)
        lockfile_path = root / PACKAGE_DIR / LOCKFILE_NAME
        lockfile = json.loads(lockfile_path.read_text(encoding="utf-8"))
        lockfile["packages"][""]["dependencies"]["leftpad"] = "1.0.0"
        lockfile_path.write_text(json.dumps(lockfile, indent=2) + "\n", encoding="utf-8")
        failures = check_vendor(root)
        expected = (
            f"{PACKAGE_DIR / LOCKFILE_NAME}: dependency leftpad is not recorded in {PACKAGE_NAME}"
        )
        if expected not in failures:
            raise RuntimeError(
                "extra-lockfile-dep fixture did not report set mismatch: "
                f"expected {expected!r} in {failures!r}"
            )


def main() -> int:
    parser = ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        help="repository root to check (default: this script's repository)",
    )
    arguments = parser.parse_args()
    repo_root = arguments.root or Path(__file__).resolve().parent.parent

    try:
        verify_fixture_reporting()
        failures = check_vendor(repo_root)
    except (OSError, RuntimeError) as error:
        print(f"dashboard-vendor: {error}", file=sys.stderr)
        return 2

    if failures:
        print(
            "vendored dashboard JS does not match crates/orbit-web/assets/dashboard/"
            "vendor/vendor-manifest.json (refresh with ./scripts/refresh-dashboard-vendor.sh):",
            file=sys.stderr,
        )
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("dashboard-vendor: vendored dashboard JS matches recorded pins")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
