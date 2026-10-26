#!/usr/bin/env bash
# Recopy dashboard vendor blobs from the npm packages pinned in package.json
# and rewrite vendor-manifest.json digests/versions plus the lockfile.
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
import hashlib
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path, PurePosixPath

repo_root = Path(sys.argv[1])
work = Path(sys.argv[2])
package_dir = repo_root / "crates" / "orbit-web" / "assets" / "dashboard"
vendor = package_dir / "vendor"
manifest_path = vendor / "vendor-manifest.json"
package_path = package_dir / "package.json"

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
package = json.loads(package_path.read_text(encoding="utf-8"))
dependencies = package["dependencies"]
env = os.environ.copy()
staging = work / "staged"
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
            staged_path.write_bytes(source.read())
        staged.append((destination, staged_path))
    digest = hashlib.sha256(staged_path.read_bytes()).hexdigest()
    asset["version"] = version
    asset["sha256"] = digest
    asset["source"] = f"https://registry.npmjs.org/{npm_package}/-/{npm_package}-{version}.tgz"
    print(f"refresh-dashboard-vendor: {asset['path']} <- {spec}:{asset['npm_file']} ({digest})")

# Every asset fetched and validated above; only now move them into place,
# manifest last, so a failure above leaves the vendor tree untouched.
for destination, staged_path in staged:
    destination.parent.mkdir(parents=True, exist_ok=True)
    staged_path.replace(destination)
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

(
  cd "$package_dir"
  npm install --package-lock-only --ignore-scripts --no-audit --no-fund
)

"$repo_root/scripts/check-dashboard-vendor.py"
