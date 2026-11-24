#!/usr/bin/env bash
# Recopy dashboard vendor blobs from the npm packages pinned in package.json
# and rewrite vendor-manifest.json digests/versions plus the lockfile. The
# blobs, manifest and lockfile are staged and checked together first, then
# published as one set: a failed step leaves all three as they were.
#
# Usage: ./scripts/refresh-dashboard-vendor.sh
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
package_dir="$repo_root/crates/orbit-web/assets/dashboard"
vendor_dir="$package_dir/vendor"
manifest="$vendor_dir/vendor-manifest.json"
package_json="$package_dir/package.json"

if [[ ! -f "$manifest" || ! -f "$package_json" ]]; then
  echo "refresh-dashboard-vendor: missing $manifest or $package_json" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "refresh-dashboard-vendor: npm is required to fetch pinned packages" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export npm_config_cache="$work/npm-cache"
export npm_config_update_notifier=false
export npm_config_fund=false
export npm_config_audit=false
mkdir -p "$npm_config_cache"

python3 - "$repo_root" "$work" <<'PY'
import errno
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath

repo_root = Path(sys.argv[1])
work = Path(sys.argv[2])
package_dir = repo_root / "crates" / "orbit-web" / "assets" / "dashboard"
vendor = package_dir / "vendor"
manifest_path = vendor / "vendor-manifest.json"
package_path = package_dir / "package.json"
lockfile_path = package_dir / "package-lock.json"

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
package = json.loads(package_path.read_text(encoding="utf-8"))
dependencies = package["dependencies"]
env = os.environ.copy()


def install_staged(staged_path, destination):
    """Move a validated staged file into place across filesystems.

    rename(2) fails with EXDEV when TMPDIR lives on another filesystem
    than the checkout, so fall back to a copy for that case only.
    """
    try:
        staged_path.replace(destination)
    except OSError as exc:
        if exc.errno != errno.EXDEV:
            raise
        shutil.copyfile(staged_path, destination)


def patch_marked_list_setext_tabs(blob):
    # Match Marked's general Setext rule in the list lazy-continuation guard,
    # and let a zero-padded ordered marker whose numeric start is 1 interrupt
    # a paragraph (CommonMark allows leading zeroes in the 1-9 digit marker).
    # Require the exact upstream bundle so the patch is reviewed on upgrade.
    upstream_sha256 = "392f4df89533b50f1dadff3f6dc0952350bd39c9f17def50a0374ef47d6147ef"
    guard = rb"/^ {0,3}(?:=+|-+) *$/"
    interrupt = rb"|1[.)]"
    if (
        hashlib.sha256(blob).hexdigest() != upstream_sha256
        or blob.count(guard) != 1
        or blob.count(interrupt) != 4
    ):
        raise SystemExit("refresh-dashboard-vendor: recheck the Marked list patches for this bundle")
    return blob.replace(guard, rb"/^ {0,3}(?:=+|-+)[ \t]*$/").replace(
        interrupt, rb"|0{0,8}1[.)]"
    )


def unchanged(step):
    return (
        f"refresh-dashboard-vendor: {step}; "
        "vendor blobs, manifest and lockfile are unchanged"
    )


# Stage replacements on the vendor filesystem so the install renames stay
# on one filesystem even when TMPDIR is elsewhere; tarballs still land in
# the TMPDIR work area. Publication below runs only after every asset was
# fetched and validated, the lockfile was generated and the staged records
# passed the vendor guard together, so a failure before it leaves the
# vendor tree and lockfile untouched.
stage_root = Path(tempfile.mkdtemp(prefix=".vendor-refresh-", dir=str(vendor)))
try:
    staging = stage_root / "staged"
    staging.mkdir()
    staged = []  # (destination, staged_path)

    for asset in manifest["assets"]:
        npm_package = asset["npm_package"]
        version = dependencies[npm_package]
        spec = f"{npm_package}@{version}"
        packed = subprocess.check_output(
            ["npm", "pack", spec, "--pack-destination", str(work), "--json"],
            env=env,
            text=True,
        )
        packed_json = json.loads(packed)
        # npm <12 emits a list of pack records; npm 12+ emits {name: record}.
        if isinstance(packed_json, list):
            records = packed_json
        elif isinstance(packed_json, dict) and "filename" in packed_json:
            records = [packed_json]
        elif isinstance(packed_json, dict):
            records = [
                value
                for value in packed_json.values()
                if isinstance(value, dict) and "filename" in value
            ]
        else:
            records = []
        if not records:
            raise SystemExit(
                f"refresh-dashboard-vendor: npm pack {spec} JSON missing filename"
            )
        filename = records[-1]["filename"]
        tarball = work / filename
        destination = vendor / asset["path"]
        with tarfile.open(tarball, "r:gz") as archive:
            # Validate the entire untrusted archive before updating any asset.
            # Reading just the requested regular file avoids filesystem extraction
            # and works even on Python versions without tarfile's data filter.
            for member in archive.getmembers():
                name = PurePosixPath(member.name)
                if name.is_absolute() or ".." in name.parts:
                    raise SystemExit(
                        f"refresh-dashboard-vendor: {spec} has unsafe member {member.name!r}"
                    )
                if not (member.isfile() or member.isdir()):
                    raise SystemExit(
                        f"refresh-dashboard-vendor: {spec} has unsupported member {member.name!r}"
                    )
            try:
                member = archive.getmember(f"package/{asset['npm_file']}")
            except KeyError:
                raise SystemExit(f"refresh-dashboard-vendor: {spec} is missing {asset['npm_file']}")
            if not member.isfile():
                raise SystemExit(f"refresh-dashboard-vendor: {spec} is missing {asset['npm_file']}")
            staged_path = staging / asset["path"]
            staged_path.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source:
                blob = source.read()
            if "patch" in asset:
                if asset["patch"] != "marked-list-setext-tabs":
                    raise SystemExit(f"refresh-dashboard-vendor: unknown patch for {spec}")
                if spec != "marked@18.2.0" or asset["npm_file"] != "lib/marked.umd.js":
                    raise SystemExit("refresh-dashboard-vendor: recheck the Marked list patches for this pin")
                blob = patch_marked_list_setext_tabs(blob)
            staged_path.write_bytes(blob)
            staged.append((destination, staged_path))
        digest = hashlib.sha256(blob).hexdigest()
        asset["version"] = version
        asset["sha256"] = digest
        asset["source"] = f"https://registry.npmjs.org/{npm_package}/-/{npm_package}-{version}.tgz"
        print(f"refresh-dashboard-vendor: {asset['path']} <- {spec}:{asset['npm_file']} ({digest})")

    staged_manifest = stage_root / manifest_path.name
    staged_manifest.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    # Generate the lockfile from a private copy of the package identity, so
    # a failed or partial npm run cannot touch the checked-in lockfile.
    lock_stage = work / "lock"
    lock_stage.mkdir()
    shutil.copyfile(package_path, lock_stage / package_path.name)
    if lockfile_path.is_file():
        shutil.copyfile(lockfile_path, lock_stage / lockfile_path.name)
    lock_command = [
        "npm", "install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund",
    ]
    status = subprocess.run(lock_command, cwd=lock_stage, env=env).returncode
    if status != 0:
        raise SystemExit(
            unchanged(f"lockfile generation failed ({' '.join(lock_command[:3])} exited {status})")
        )
    if not (lock_stage / lockfile_path.name).is_file():
        raise SystemExit(
            unchanged(f"lockfile generation wrote no {lockfile_path.name} ({' '.join(lock_command[:3])})")
        )
    staged_lockfile = stage_root / lockfile_path.name
    shutil.copyfile(lock_stage / lockfile_path.name, staged_lockfile)

    # Check the staged blobs, manifest, package.json and lockfile against
    # each other with the vendor guard before publishing any of them.
    verify_root = work / "verify"
    verify_package = verify_root / package_dir.relative_to(repo_root)
    verify_vendor = verify_package / vendor.name
    verify_vendor.mkdir(parents=True)
    shutil.copyfile(package_path, verify_package / package_path.name)
    shutil.copyfile(staged_lockfile, verify_package / lockfile_path.name)
    shutil.copyfile(staged_manifest, verify_vendor / manifest_path.name)
    for destination, staged_path in staged:
        shutil.copyfile(staged_path, verify_vendor / destination.name)
    guard = repo_root / "scripts" / "check-dashboard-vendor.py"
    status = subprocess.run([sys.executable, str(guard), "--root", str(verify_root)]).returncode
    if status != 0:
        raise SystemExit(unchanged(f"staged records failed {guard.name} (exit {status})"))

    # Publish the verified set. A failure part-way restores every record
    # already replaced, so the three stay mutually consistent either way.
    backups = stage_root / "backups"
    backups.mkdir()
    replaced = []  # (destination, backup or None)
    try:
        for destination, staged_path in staged + [
            (manifest_path, staged_manifest),
            (lockfile_path, staged_lockfile),
        ]:
            destination.parent.mkdir(parents=True, exist_ok=True)
            backup = None
            if destination.exists():
                backup = backups / str(len(replaced))
                shutil.copyfile(destination, backup)
            replaced.append((destination, backup))
            install_staged(staged_path, destination)
    except BaseException as error:
        for destination, backup in reversed(replaced):
            if backup is None:
                destination.unlink(missing_ok=True)
            else:
                shutil.copyfile(backup, destination)
        print(unchanged(f"publication failed ({error}) and was rolled back"), file=sys.stderr)
        raise
finally:
    shutil.rmtree(stage_root, ignore_errors=True)
PY

"$repo_root/scripts/check-dashboard-vendor.py"
