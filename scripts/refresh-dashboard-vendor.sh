#!/usr/bin/env bash
# Recopy dashboard vendor blobs from the npm packages pinned in package.json
# and rewrite vendor-manifest.json digests/versions plus the lockfile.
#
# Usage: ./scripts/refresh-dashboard-vendor.sh
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
vendor_dir="$repo_root/crates/orbit-web/assets/dashboard"
manifest="$vendor_dir/vendor-manifest.json"
package_json="$vendor_dir/package.json"

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
from pathlib import Path

repo_root = Path(sys.argv[1])
work = Path(sys.argv[2])
vendor = repo_root / "crates" / "orbit-web" / "assets" / "dashboard"
manifest_path = vendor / "vendor-manifest.json"
package_path = vendor / "package.json"

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
package = json.loads(package_path.read_text(encoding="utf-8"))
dependencies = package["dependencies"]
env = os.environ.copy()

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
    extract = work / f"extract-{npm_package}"
    extract.mkdir()
    with tarfile.open(tarball, "r:gz") as archive:
        try:
            archive.extractall(extract, filter="data")
        except TypeError:
            archive.extractall(extract)
    source = extract / "package" / asset["npm_file"]
    destination = vendor / asset["path"]
    if not source.is_file():
        raise SystemExit(f"refresh-dashboard-vendor: {spec} is missing {asset['npm_file']}")
    destination.write_bytes(source.read_bytes())
    digest = hashlib.sha256(destination.read_bytes()).hexdigest()
    asset["version"] = version
    asset["sha256"] = digest
    asset["source"] = f"https://registry.npmjs.org/{npm_package}/-/{npm_package}-{version}.tgz"
    print(f"refresh-dashboard-vendor: {asset['path']} <- {spec}:{asset['npm_file']} ({digest})")

manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

(
  cd "$vendor_dir"
  npm install --package-lock-only --ignore-scripts --no-audit --no-fund
)

"$repo_root/scripts/check-dashboard-vendor.py"
